# 2026-11-22: Full cost matrix and test262 baseline

## Setup

- BASE is `852c7036b9d0342406824780452e3f171ae572bf`, the main commit measured as NEW in the [previous full matrix](2026-11-21-full-matrix.md). NEW is `e3817f63d72c3ed5477fc54b2d638e990577a0b6` (#923). The range contains 59 first-parent merges.
- archMachine uses an AMD Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen and Rust 1.96.0.
- Build each tree in its own target directory with `CARGO_BUILD_JOBS=6`. Build the `bare-js`, `bare` and `bare-js2` release examples and `bare-stress`. Build the four js2 examples that predate the rename separately on BASE. Copy them under their current names so the old-engine examples keep their own binaries.
- Run `tools/js/cost_matrix.sh -j4 -a -o matrix-default.tsv BASE_DIR NEW_DIR`. Report the minimum of three `perf stat -e instructions:u` counts per row, tree and allocator. The script sets `glibc.malloc.tcache_count=65535` and runs jemalloc when glibc exceeds +0.5%.
- Rebuild the 24 flagged rows with `RUSTFLAGS="-C codegen-units=1"` on both trees. Sample both allocators three times. Use the minimum for the delta and max minus min for the spread.
- Measure the five `interp_load` micros separately. Subtract the minimum with input zero from the minimum with the listed input. For comparisons with earlier PRs, repeat their original allocator settings and count the whole process.

## Default matrix

The default matrix measured 821 of 823 rows in 116 s. Mean glibc change was -1.114%. Thirteen rows exceeded +0.5% under both allocators. Eleven exceeded it under glibc only.

`parse_load deep 1` and `parse_load deep 10` overflow the stack on BASE. NEW completes both. Their percentage change is unavailable.

## One-codegen-unit recheck

All percentages give glibc / jemalloc. The spread is the largest difference in instructions between three samples per tree and allocator.

| Row | Default change | One-codegen-unit change | Largest spread |
| --- | ---: | ---: | ---: |
| `SELECT_LOAD_STACK_MIB=8 select_load value 1000` | +1.467% / +1.325% | +0.094% / +0.081% | 27,721 |
| `bare-stress run block-deep 40` | +0.598% / +0.135% | +0.704% / +0.228% | 1,455 |
| `bare-stress run block-nested-collapse 20` | +0.630% / +0.140% | +0.740% / +0.226% | 1,749 |
| `bare-stress run inline-blocks-nested 40` | +0.521% / +0.129% | +0.593% / +0.203% | 1,164 |
| `bare-stress run inline-deep-spans-many-lines 100` | +0.507% / +0.130% | +0.628% / +0.209% | 1,449 |
| `bare-stress run overflow-nested-auto 20` | +0.618% / +0.129% | +0.743% / +0.250% | 918 |
| `browser_load mutation 1` | +1.093% / +1.014% | +0.023% / +0.026% | 1,277 |
| `browser_load mutation 10` | +1.387% / +1.357% | -0.050% / -0.064% | 270,118 |
| `browser_load rel 1` | +0.905% / +0.811% | -0.101% / -0.092% | 28,498 |
| `browser_load rel 10` | +1.110% / +1.077% | -0.116% / -0.117% | 197,656 |
| `browser_load svg 1` | +0.857% / +0.804% | +0.119% / +0.076% | 51,930 |
| `browser_load svg 10` | +0.918% / +0.889% | +0.095% / +0.089% | 326,933 |
| `js2_lex_load small 1 1` | +0.919% / +0.871% | -0.000% / -0.007% | 3,052 |
| `js2_parse_load full stmts 1` | +0.575% / +0.387% | -0.243% / -0.195% | 7,539 |
| `observer_load mo 1` | +0.839% / +0.765% | +0.041% / +0.010% | 45,385 |
| `observer_load mo 10` | +0.910% / +0.890% | +0.053% / +0.046% | 1,004,707 |
| `platform_load class 1` | +0.566% / +0.519% | +0.067% / +0.109% | 25,001 |
| `platform_load class 10` | +0.662% / +0.650% | +0.097% / +0.106% | 216,049 |
| `preload_load dups 1` | +0.710% / +0.160% | +0.362% / +0.329% | 95,083 |
| `preload_load font 1` | +0.600% / +0.020% | +0.636% / +0.198% | 131,309 |
| `preload_load script 1` | +0.613% / -0.410% | +0.265% / +0.203% | 106,791 |
| `preload_load style 1` | +0.537% / +0.272% | +0.463% / +0.271% | 106,967 |
| `resources_load small 1` | +0.608% / +0.184% | +1.010% / +0.337% | 113,335 |
| `session_load chain 1` | +1.424% / +0.626% | +1.410% / +0.838% | 134,098 |

## js2 controls

| Row | Default change | One-codegen-unit change |
| --- | ---: | ---: |
| `interp_load loopfn 3000000` | -9.814% / -9.813% | -5.997% / -5.996% |
| `interp_load calls 1000000` | -6.560% / -6.560% | -1.768% / -1.768% |
| `interp_load fib 27` | -8.002% / -8.000% | -3.065% / -3.065% |
| `interp_load closures 1000000` | -11.326% / -11.322% | -9.202% / -9.200% |
| `interp_load objects 1000000` | -1.885% / -1.889% | -1.886% / -1.890% |
| `script_run (var x = 1;)` | -63.489% / -25.035% | -63.244% / -24.859% |

The five interpreter rows and startup pass in both build modes. The largest conservative spread of the setup-subtracted controls is 65,086 instructions.

`js2_gen_load next 100000` is -1.393% on default glibc, so the matrix skips jemalloc. The extra one-codegen-unit control gives +2.538% / +2.467%. The one-codegen-unit control also has a first-parent threshold crossing. The build at `cde27a0d` (#870) gives +0.470% / +0.442% against BASE. Its successor `adcfe840` (#877) gives +0.653% / +0.621% against BASE, and +0.182% / +0.178% against #870. The default-build rule passes.

## Bisect of `session_load chain 1`

This is the only row over +0.5% under both allocators in both builds. Four extra one-codegen-unit batches reproduced the BASE-to-NEW increase. Glibc deltas ranged from +1.407% to +1.424%. Jemalloc deltas ranged from +0.856% to +0.873%.

Build and sample the row on the first-parent history. Each cell gives the minimum of three runs at one codegen unit. The loopback server sometimes changes the count by about 0.1 M instructions. Use both allocators for the threshold decision.

| Commit | Glibc instructions | Jemalloc instructions |
| --- | ---: | ---: |
| `852c7036` (BASE) | 4,939,026 | 8,816,272 |
| `1303414b` (#850) | 4,946,057 | 8,918,651 |
| `da9ba9af` (#856) | 4,947,460 | 8,921,919 |
| `1db951bd` (#851 / #854) | 5,005,405 | 8,980,793 |
| `52bf1db5` (#857) | 5,005,682 | 8,980,540 |
| `7f558ea8` (#879) | 5,006,697 | 8,981,930 |
| `9c8e9f28` (#910) | 5,010,915 | 8,986,147 |
| `8a06e07d` (#912) | 5,009,892 | 8,888,130 |
| `e3817f63` (NEW) | 5,008,642 | 8,890,185 |

The threshold crossing is `1db951bd`. The merge message names #854. GitHub records the same merge commit for #851 and #854. The direct change from the first parent (`da9ba9af`) is +1.171% / +0.660%. The [#851 cost report](2026-11-21-s04-pr6c2-js2-browser.md#cost-rows) measured the same row and identified the extra page-loop branch and the change from `Browser<Old>` to `AnyBrowser`. Hypothesis: the new session dispatch and browser wrapper increase instructions on the old-engine path. The bisect locates the merge. Profiling would be needed to isolate the branch cost.

## Debt from earlier PRs

Compare each row with the reference from its PR. Percentages give glibc / jemalloc from forced one-codegen-unit builds.

| PR | Row | Reference | NEW change | State |
| --- | --- | --- | ---: | --- |
| #876 | `interp_load loopfn 3000000` | `c81eb892` | -1.113% / -1.113% | Fixed |
| #876 | `interp_load calls 1000000` | `c81eb892` | +1.833% / +1.833% | Open |
| #864 | `raw_collect 10000 1000 10` | #864's reported reference | +4.483% / +4.208% | Open |
| #901 | `script_run` startup, `var x = 1;` | #901's comparison against #882 | -0.497% / -8.277% | Fixed |
| #921 | `interp_load fib 27` | #921's recorded pre-change counts | -2.683% / -2.661% | Fixed |
| #922 | `interp_load calls 1000000` | `2be8e4a9` | -1.416% / -1.408% | Fixed |

The #876 recheck uses fresh builds and subtracts the setup count on both sides. The calls row passes against the BASE of this matrix and against the #922 reference. It still exceeds the earlier #876 reference. The [#896 report](2026-11-21-cost-debt-js2-startup-loop.md) measured a recovery in a separate tree.

The #864 report substitutes `raw_collect(&[])` for the regular collection call in `owner_load`. Repeat that substitution in scratch worktrees. NEW counts 63,117,585 on glibc and 66,500,060 on jemalloc at one codegen unit. The original report lists 60,409,620 and 63,814,640. A fresh build of the first-parent predecessor of the #864 merge (`0c4db75c`) gives 80,437,262 and 83,951,923. NEW passes against that later predecessor. NEW still exceeds the rule against the original reference.

The [#911 report](2026-11-22-js2-adaptive-gc-trigger.md#cost-debt-at-the-original-head) lists three over-rule rows at its original head. The merged follow-up gives glibc / jemalloc changes of +0.272% / +0.262% for `closures_10`, +0.299% / +0.287% for `js2_promise_load then 100000`, and +0.227% / +0.157% for `heap_load task 1000000`. [#913](2026-11-22-js2-root-liveness.md#cost-debt) records a verifier-to-compile ratio of 25.64% against a 25% target. Its instruction rows passed. The HTTP cache and portable-cache progress files list zero over-rule rows.

## Test262

Run `TEST262=/data/claude-tmp/test262 tools/js/test262_js2.sh -j 4` at suite pin `e4fbf57745c6aae3d1f12f181ecef78b1665d812`. Use a dedicated copy of the release runner from `bare-js-diff` and a 300 s directory timeout. The first run shared the example output name `test262_run` with `bare-js` and produced invalid results after the binary was replaced. The verified run took 493 s. It recorded a result for all 39,373 tests in the 986 directories on each engine and reported zero process deaths or timeouts.

| Engine | Previous pass / fail / skip | NEW pass / fail / skip | Gained passes | Lost passes |
| --- | ---: | ---: | ---: | ---: |
| bare-js | 26,332 / 1,896 / 8,173 | 27,748 / 3,088 / 8,537 | 1,416 | 0 |
| bare-js2 | 27,015 / 1,213 / 8,173 | 29,217 / 1,619 / 8,537 | 2,202 | 0 |

The archived #872 per-test run reproduces all 787 directory rows of the previous fixture. Compare the result by test path on each engine. The added directories contribute 1,375 passes on bare-js and 1,377 on bare-js2. There are no lost passes on either engine. At NEW, 391 tests pass on bare-js and fail on bare-js2.

The refresh uses the flat baseline tracked on main. Copy the generated `counts.txt` into that fixture with its header. The current `-u` option writes per-area files.

## Not tested

Windows, Miri, browser journeys and wall-time performance. The measurement ends at the pinned NEW commit.

## All default matrix rows

<details>
<summary>Instruction minima by allocator</summary>

A dash means the script skipped jemalloc. FAIL means the binary did not complete.

```text
row	glibc BASE	glibc NEW	jemalloc BASE	jemalloc NEW
FORM_LOAD_STACK_MIB=8 examples/form_load gates 1	127567521	123254240	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load gates 10	1213625365	1168946522	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load gc 1	109767466	100081573	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load gc 10	1044286222	947238473	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load submit 1	33929679	33970357	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load submit 10	274947513	275098824	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load validity 1	107074889	104686433	-	-
FORM_LOAD_STACK_MIB=8 examples/form_load validity 10	1030973923	1006859761	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load churn 10	30603292	30620986	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load churn 1000	320977611	321714343	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load react 10	22971610	22991245	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load react 1000	1140461279	1141662786	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load value 10	20822655	20917701	-	-
SELECT_LOAD_STACK_MIB=8 examples/select_load value 1000	86095890	87359295	92370627	93594191
bare-stress run block-collapse-through 20000	119503240	119514828	-	-
bare-stress run block-collapse-through 200000	1179132763	1179144598	-	-
bare-stress run block-deep 40	1960464	1972184	8498197	8509684
bare-stress run block-deep 400	3770918	3782703	-	-
bare-stress run block-deep-over-limit 2000	6869034	6895741	-	-
bare-stress run block-deep-over-limit 20000	42353033	42364893	-	-
bare-stress run block-list-items 20000	248234239	248256871	-	-
bare-stress run block-list-items 200000	2467433783	2467558324	-	-
bare-stress run block-nested-collapse 20	1880783	1892626	8422950	8434754
bare-stress run block-nested-collapse 200	2992975	3005235	-	-
bare-stress run block-percent-calc 20000	119361253	119373499	-	-
bare-stress run block-percent-calc 200000	1177551271	1177562685	-	-
bare-stress run block-wide 20000	119025221	119036762	-	-
bare-stress run block-wide 200000	1174334935	1174346378	-	-
bare-stress run boxes-deep-blocks 2000	6881337	6908561	-	-
bare-stress run boxes-deep-blocks 20000	53076035	53087176	-	-
bare-stress run boxes-deep-inline 2000	5999058	6010809	-	-
bare-stress run boxes-deep-inline 20000	44239254	44251371	-	-
bare-stress run boxes-display-none 20000	312705324	312718919	-	-
bare-stress run boxes-display-none 200000	3110771329	3110785338	-	-
bare-stress run boxes-inline-blocks 20000	104056265	103988124	-	-
bare-stress run boxes-inline-blocks 200000	1023692917	1023704528	-	-
bare-stress run boxes-list-items 20000	79512694	79524189	-	-
bare-stress run boxes-list-items 200000	778994145	779006503	-	-
bare-stress run boxes-mixed-runs 20000	115423512	115434806	-	-
bare-stress run boxes-mixed-runs 200000	1138909564	1138921232	-	-
bare-stress run boxes-one-long-run 20000	110371342	110383202	-	-
bare-stress run boxes-one-long-run 200000	1087522017	1087534697	-	-
bare-stress run boxes-spaces-between 20000	113815024	113825997	-	-
bare-stress run boxes-spaces-between 200000	1122146415	1122157664	-	-
bare-stress run boxes-wide-blocks 20000	89768239	89780112	-	-
bare-stress run boxes-wide-blocks 200000	881793266	881804849	-	-
bare-stress run flex-cards-percent-height 2000	106609664	106622990	-	-
bare-stress run flex-cards-percent-height 20000	1052896997	1052988031	-	-
bare-stress run flex-cards-stretch 2000	101132988	101153010	-	-
bare-stress run flex-cards-stretch 20000	995181667	995273346	-	-
bare-stress run flex-column-many-items 2000	22920229	22932225	-	-
bare-stress run flex-column-many-items 20000	213051930	213063758	-	-
bare-stress run flex-freeze-many 2000	41559646	41568205	-	-
bare-stress run flex-freeze-many 20000	407786564	407798105	-	-
bare-stress run flex-nested-columns 200	3324781	3336860	-	-
bare-stress run flex-nested-columns 2000	8282157	8300826	-	-
bare-stress run flex-nested-rows 200	3716057	3727293	-	-
bare-stress run flex-nested-rows 2000	9075158	9077935	-	-
bare-stress run flex-nested-rows-text 2000	4237779	4249573	-	-
bare-stress run flex-nested-rows-text 20000	23912665	23924083	-	-
bare-stress run flex-ref-block-items 2000	20702494	20714418	-	-
bare-stress run flex-ref-block-items 20000	191062321	191073606	-	-
bare-stress run flex-ref-block-nested-text 2000	2947196	2958310	-	-
bare-stress run flex-ref-block-nested-text 20000	12252144	12263306	-	-
bare-stress run flex-row-many-items 2000	27681347	27693291	-	-
bare-stress run flex-row-many-items 20000	260724694	260816435	-	-
bare-stress run flex-wrap-many-items 2000	26430431	26442177	-	-
bare-stress run flex-wrap-many-items 20000	248217771	248229461	-	-
bare-stress run floats-alternate-clear 2000	37634122	37628312	-	-
bare-stress run floats-alternate-clear 20000	365346597	365358716	-	-
bare-stress run floats-in-a-line 2000	29575432	29587433	-	-
bare-stress run floats-in-a-line 20000	282166730	282178068	-	-
bare-stress run floats-new-contexts 2000	41115369	41130429	-	-
bare-stress run floats-new-contexts 20000	399656177	399668132	-	-
bare-stress run floats-one-tall 20000	14169772	14180636	-	-
bare-stress run floats-one-tall 200000	125531402	125543725	-	-
bare-stress run floats-stack 2000	15374392	15393925	-	-
bare-stress run floats-stack 20000	140471410	140483118	-	-
bare-stress run floats-with-paragraphs 2000	43055621	43073147	-	-
bare-stress run floats-with-paragraphs 20000	419820016	419911306	-	-
bare-stress run grid-absolute-children 2000	41413620	41425924	-	-
bare-stress run grid-absolute-children 20000	395568846	395500817	-	-
bare-stress run grid-auto-columns-content 5000	70346869	70358454	-	-
bare-stress run grid-auto-columns-content 50000	686867691	686879535	-	-
bare-stress run grid-auto-fill 5000	60903320	60913669	-	-
bare-stress run grid-auto-fill 50000	592276817	592288120	-	-
bare-stress run grid-column-flow 5000	72262839	72275045	-	-
bare-stress run grid-column-flow 50000	705315175	705326665	-	-
bare-stress run grid-dense 5000	123963773	123995570	-	-
bare-stress run grid-dense 50000	1210316025	1210327696	-	-
bare-stress run grid-equal-columns 5000	77538292	77549452	-	-
bare-stress run grid-equal-columns 50000	757823965	757835599	-	-
bare-stress run grid-explicit-rows 5000	129275907	129289199	-	-
bare-stress run grid-explicit-rows 50000	1252790620	1252704013	-	-
bare-stress run grid-inline-grids 2000	110754446	110766250	-	-
bare-stress run grid-inline-grids 20000	1090329261	1090340635	-	-
bare-stress run grid-many-grids 2000	140426669	140437417	-	-
bare-stress run grid-many-grids 20000	1388200517	1388211855	-	-
bare-stress run grid-nested 2000	228912802	228924846	-	-
bare-stress run grid-nested 20000	2269808741	2269820235	-	-
bare-stress run grid-spanning-items 5000	214788235	214799969	-	-
bare-stress run grid-spanning-items 50000	2116148925	2116160900	-	-
bare-stress run grid-sparse-spans 5000	119351819	119363176	-	-
bare-stress run grid-sparse-spans 50000	1165614042	1165625844	-	-
bare-stress run grid-stretch-boxes 5000	220424083	220433203	-	-
bare-stress run grid-stretch-boxes 50000	2185450765	2185328371	-	-
bare-stress run inline-block-in-inline 20000	204193567	204227347	-	-
bare-stress run inline-block-in-inline 200000	2025903711	2026132476	-	-
bare-stress run inline-blocks-many 20000	270805117	270816634	-	-
bare-stress run inline-blocks-many 200000	2691355399	2691367442	-	-
bare-stress run inline-blocks-nested 40	2306769	2318795	8849804	8861221
bare-stress run inline-blocks-nested 400	5925819	5936018	-	-
bare-stress run inline-breaks 20000	91446112	91458123	-	-
bare-stress run inline-breaks 200000	898386547	898399108	-	-
bare-stress run inline-deep-spans 2000	8520261	8532339	-	-
bare-stress run inline-deep-spans 20000	69321582	69333050	-	-
bare-stress run inline-deep-spans-many-lines 100	2195961	2207089	8732835	8744229
bare-stress run inline-deep-spans-many-lines 1000	6109274	6120572	-	-
bare-stress run inline-list 20000	178629965	178641761	-	-
bare-stress run inline-list 200000	1770070865	1770082496	-	-
bare-stress run inline-long-word 200000	6213163	6224060	-	-
bare-stress run inline-long-word 2000000	46320172	46332424	-	-
bare-stress run inline-many-spans 20000	324933826	324945410	-	-
bare-stress run inline-many-spans 200000	3233034656	3233047541	-	-
bare-stress run inline-one-line-nowrap 20000	8487440	8499150	-	-
bare-stress run inline-one-line-nowrap 200000	69038385	69049889	-	-
bare-stress run inline-paragraph 20000	13251612	13263730	-	-
bare-stress run inline-paragraph 200000	116618152	116629695	-	-
bare-stress run inline-pre-lines 20000	24599355	24612015	-	-
bare-stress run inline-pre-lines 200000	230077554	230089828	-	-
bare-stress run inline-span-unit 20000	131549351	131560550	-	-
bare-stress run inline-span-unit 200000	1298942282	1298953842	-	-
bare-stress run overflow-auto-fits 2000	35150569	35163436	-	-
bare-stress run overflow-auto-fits 20000	335040025	335051615	-	-
bare-stress run overflow-long-log 20000	223826409	223758251	-	-
bare-stress run overflow-long-log 200000	2220873995	2220885667	-	-
bare-stress run overflow-many-auto 2000	49796295	49808003	-	-
bare-stress run overflow-many-auto 20000	481464115	481475109	-	-
bare-stress run overflow-nested-auto 20	1906357	1918144	8451652	8462552
bare-stress run overflow-nested-auto 200	2926494	2938107	-	-
bare-stress run overflow-wide-content 2000	3062759	3073596	-	-
bare-stress run overflow-wide-content 20000	14556371	14567996	-	-
bare-stress run position-absolute-in-each-relative 2000	41537699	41539843	-	-
bare-stress run position-absolute-in-each-relative 20000	399124938	399216407	-	-
bare-stress run position-fixed-deep 300	3670110	3679582	-	-
bare-stress run position-fixed-deep 3000	9346755	9358931	-	-
bare-stress run position-many-absolute 2000	34821450	34833121	-	-
bare-stress run position-many-absolute 20000	331892695	331824176	-	-
bare-stress run position-nested-relative 300	3337338	3346414	-	-
bare-stress run position-nested-relative 3000	9347642	9359294	-	-
bare-stress run position-relative-spans 2000	22690931	22702822	-	-
bare-stress run position-relative-spans 20000	210476446	210328115	-	-
bare-stress run position-shrink-to-fit 2000	40496516	40508113	-	-
bare-stress run position-shrink-to-fit 20000	388937361	388950365	-	-
bare-stress run table-collapse 10000	376729188	376740914	-	-
bare-stress run table-collapse 100000	3752066060	3751677369	-	-
bare-stress run table-colspan-chain 1000	31803118	31812802	-	-
bare-stress run table-colspan-chain 10000	319309082	319344002	-	-
bare-stress run table-fixed-layout 20000	613251214	613262590	-	-
bare-stress run table-fixed-layout 200000	6116765547	6116778109	-	-
bare-stress run table-many-columns 5000	61208453	61220626	-	-
bare-stress run table-many-columns 50000	322629760	322641731	-	-
bare-stress run table-many-rows 20000	730735019	730746305	-	-
bare-stress run table-many-rows 200000	7291451610	7293360965	-	-
bare-stress run table-many-tables 2000	123974889	123987104	-	-
bare-stress run table-many-tables 20000	1223532491	1223632144	-	-
bare-stress run table-nested 150	6490962	6501282	-	-
bare-stress run table-nested 1500	20460083	20477573	-	-
bare-stress run table-percent-columns 10000	570785793	570757652	-	-
bare-stress run table-percent-columns 100000	5691501094	5691512031	-	-
bare-stress run table-rowspan-grid 5000	423065203	423042592	-	-
bare-stress run table-rowspan-grid 50000	4213715855	4213988406	-	-
bare-stress run text-accents 100000	102608871	102621068	-	-
bare-stress run text-accents 1000000	1017208389	1017220482	-	-
bare-stress run text-chinese 100000	62080754	62092991	-	-
bare-stress run text-chinese 1000000	611926902	611938705	-	-
bare-stress run text-one-letter-words 1000000	179174802	179186493	-	-
bare-stress run text-one-letter-words 10000000	1782867282	1782878179	-	-
bare-stress run text-one-long-word 1000000	17081010	17092153	-	-
bare-stress run text-one-long-word 10000000	159105342	159116884	-	-
bare-stress run text-only-spaces 1000000	28268879	28280717	-	-
bare-stress run text-only-spaces 10000000	273807017	273816709	-	-
bare-stress run text-pre-spaces 1000000	7081667	7092784	-	-
bare-stress run text-pre-spaces 10000000	61927385	61939601	-	-
bare-stress run text-pre-tabs 1000000	102174923	102187119	-	-
bare-stress run text-pre-tabs 10000000	1012867487	1012878862	-	-
bare-stress run text-uppercase 100000	94862799	94874360	-	-
bare-stress run text-uppercase 1000000	939739773	939751130	-	-
bare-stress run text-words 100000	50299332	50310997	-	-
bare-stress run text-words 1000000	494114822	494126359	-	-
examples/adapter_load asynciter 1	11597767	11597589	-	-
examples/adapter_load asynciter 10	76002352	76001219	-	-
examples/adapter_load codec 1	23905252	23904870	-	-
examples/adapter_load codec 10	205781362	205780970	-	-
examples/adapter_load suspiter 1	25578553	25577620	-	-
examples/adapter_load suspiter 10	221636784	221636829	-	-
examples/adapter_load u8iter 1	14353556	14352867	-	-
examples/adapter_load u8iter 10	104287907	104287827	-	-
examples/adapter_load u8steps 1	69251060	69250397	-	-
examples/adapter_load u8steps 10	661114190	661114351	-	-
examples/arrobj_load arrays 1	16682783	16682944	-	-
examples/arrobj_load arrays 10	115171518	115170764	-	-
examples/arrobj_load descriptors 1	7685332	7685289	-	-
examples/arrobj_load descriptors 10	48543810	48543332	-	-
examples/arrobj_load fill 1	23271435	23271225	-	-
examples/arrobj_load fill 10	205598174	205597678	-	-
examples/arrobj_load flat 1	41415823	41415752	-	-
examples/arrobj_load flat 10	385801002	385800656	-	-
examples/arrobj_load freeze 1	141011903	141012262	-	-
examples/arrobj_load freeze 10	1376690752	1376689384	-	-
examples/arrobj_load lastindexof 1	262412977	262412409	-	-
examples/arrobj_load lastindexof 10	2603328496	2603328853	-	-
examples/arrobj_load mapfilter 1	22132934	22132965	-	-
examples/arrobj_load mapfilter 10	193759733	193759348	-	-
examples/arrobj_load objects 1	8316416	8316104	-	-
examples/arrobj_load objects 10	54840437	54840279	-	-
examples/arrobj_load pad 1	4953212	4952931	-	-
examples/arrobj_load pad 10	22353894	22353656	-	-
examples/arrobj_load queue 1	15175552	15175544	-	-
examples/arrobj_load queue 10	208383440	208383703	-	-
examples/arrobj_load repeat 1	3045400	3045531	-	-
examples/arrobj_load repeat 10	3384415	3384587	-	-
examples/arrobj_load replaceall 1	31360653	31360338	-	-
examples/arrobj_load replaceall 10	286348969	286348518	-	-
examples/arrobj_load reverse 1	198612452	198612090	-	-
examples/arrobj_load reverse 10	1966208955	1966208660	-	-
examples/arrobj_load sort 1	59194323	59194779	-	-
examples/arrobj_load sort 10	654038845	654037957	-	-
examples/arrobj_load tosorted 1	51547796	51547762	-	-
examples/arrobj_load tosorted 10	577485333	577485274	-	-
examples/async_load closures 1	8833504	8833370	-	-
examples/async_load closures 10	56450096	56450003	-	-
examples/async_load fanout 1	9086767	9086386	-	-
examples/async_load fanout 10	59023175	59023190	-	-
examples/async_load ifchain 1	10413390	10413934	-	-
examples/async_load ifchain 10	71943696	71942321	-	-
examples/async_load nested 1	9471125	9471387	-	-
examples/async_load nested 10	61223117	61222712	-	-
examples/async_load pending 1	8257742	8257283	-	-
examples/async_load pending 10	50381442	50380962	-	-
examples/async_load seq 1	8422419	8422143	-	-
examples/async_load seq 10	52937624	52936293	-	-
examples/async_load switch 1	10174898	10174132	-	-
examples/async_load switch 10	69957118	69957192	-	-
examples/async_load trycatch 1	10319432	10317230	-	-
examples/async_load trycatch 10	71015667	71015408	-	-
examples/attr_load churn 1	96000050	96152380	-	-
examples/attr_load churn 10	899874169	900928479	-	-
examples/attr_load live 1	29607300	29621683	-	-
examples/attr_load live 10	181379453	181642814	-	-
examples/attr_load react 1	216598787	217003373	-	-
examples/attr_load react 10	2092855185	2096839119	-	-
examples/bigint_load divmod 1	3351772	3351845	-	-
examples/bigint_load divmod 10	19720407	19720728	-	-
examples/bigint_load heapmix 1	252831122	252831383	-	-
examples/bigint_load heapmix 10	2441391931	2441392197	-	-
examples/bigint_load i64sort 1	368423247	368423044	-	-
examples/bigint_load i64sort 10	3702985212	3702984974	-	-
examples/bigint_load mul 1	3558978	3558753	-	-
examples/bigint_load mul 10	29239093	29239073	-	-
examples/bigint_load small 1	378016129	378015682	-	-
examples/bigint_load small 10	3753466146	3753466145	-	-
examples/bigint_load tostring 1	3164241	3164390	-	-
examples/bigint_load tostring 10	9151950	9152145	-	-
examples/bigint_load u64sort 1	634465449	634465240	-	-
examples/bigint_load u64sort 10	6359846379	6359847573	-	-
examples/binary_load codec 1	44591718	44591163	-	-
examples/binary_load codec 10	419320252	419320314	-	-
examples/binary_load copy 1	5407217	5407225	-	-
examples/binary_load copy 10	27127978	27126932	-	-
examples/binary_load dataview 1	880696539	880696315	-	-
examples/binary_load dataview 10	8779803072	8779803048	-	-
examples/binary_load f64from 1	82458627	82458611	-	-
examples/binary_load f64from 10	797267308	797267317	-	-
examples/binary_load i32sort 1	78214934	78215015	-	-
examples/binary_load i32sort 10	702170241	702170668	-	-
examples/binary_load indexof 1	48485091	48485348	-	-
examples/binary_load indexof 10	457849635	457849178	-	-
examples/binary_load scan 1	345953618	345953868	-	-
examples/binary_load scan 10	3432812720	3432812718	-	-
examples/binary_load scanarr 1	914051816	914052462	-	-
examples/binary_load scanarr 10	9113034668	9113033859	-	-
examples/binary_load stream 1	38500757	38500259	-	-
examples/binary_load stream 10	357322432	357322590	-	-
examples/browser_load event 1	32304252	32142923	-	-
examples/browser_load event 10	274143733	271875479	-	-
examples/browser_load lookup 1	85261633	85158683	-	-
examples/browser_load lookup 10	796160271	794912509	-	-
examples/browser_load mutation 1	58533196	59172680	65603406	66268304
examples/browser_load mutation 10	575374284	583353445	585166158	593108749
examples/browser_load rel 1	67946229	68561340	75709049	76322972
examples/browser_load rel 10	732529997	740660077	753482673	761596383
examples/browser_load svg 1	136567374	137738355	144097675	145256790
examples/browser_load svg 10	1500089975	1513862868	1513123363	1526577892
examples/browser_load svgparse 1	33072150	33127888	-	-
examples/browser_load svgparse 10	254617347	254849311	-	-
examples/builtins_load agents 1	48915687	48915910	-	-
examples/builtins_load agents 10	165991393	165991959	-	-
examples/builtins_load gc 1	78686913	78686630	-	-
examples/builtins_load gc 10	792570355	792570203	-	-
examples/builtins_load json 1	23429819	23429925	-	-
examples/builtins_load json 10	212720804	212720814	-	-
examples/builtins_load replace 1	62557906	62557324	-	-
examples/builtins_load replace 10	640638515	640638886	-	-
examples/builtins_load routes 1	162436259	162435965	-	-
examples/builtins_load routes 10	1616657527	1616658208	-	-
examples/builtins_load scan 1	61553199	61553267	-	-
examples/builtins_load scan 10	614832807	614831251	-	-
examples/builtins_load split 1	31823865	31823739	-	-
examples/builtins_load split 10	293091124	293091562	-	-
examples/builtins_load strings 1	248303761	248303591	-	-
examples/builtins_load strings 10	2474414894	2474415297	-	-
examples/builtins_load zero 1	43047752	43047245	-	-
examples/builtins_load zero 10	405847235	405847272	-	-
examples/class_load accessors 1	241747321	241747472	-	-
examples/class_load accessors 10	2393609399	2393609327	-	-
examples/class_load churn 1	173811212	173810626	-	-
examples/class_load churn 10	1709295699	1709293410	-	-
examples/class_load construct 1	159659004	159658785	-	-
examples/class_load construct 10	1572880046	1572879832	-	-
examples/class_load derived 1	227530256	227529652	-	-
examples/class_load derived 10	2246936267	2246936059	-	-
examples/class_load private 1	192927346	192927598	-	-
examples/class_load private 10	1905412825	1905412486	-	-
examples/class_load super 1	175667885	175668429	-	-
examples/class_load super 10	1733104839	1733104489	-	-
examples/console_load flood 1	19902138	19949597	-	-
examples/console_load flood 10	139165110	139588546	-	-
examples/console_load format 1	11512968	11516137	-	-
examples/console_load format 10	62360133	62281554	-	-
examples/console_load patch 1	22248778	22292668	-	-
examples/console_load patch 10	162159331	162514666	-	-
examples/console_load retain 1	17049489	17071764	-	-
examples/console_load retain 10	130783275	130908590	-	-
examples/cssom_load adopted 1	89063547	88925770	-	-
examples/cssom_load adopted 10	826976460	825510035	-	-
examples/cssom_load computed 1	275177222	275737538	-	-
examples/cssom_load computed 10	2659852595	2665230460	-	-
examples/cssom_load rules 1	154062007	151265882	-	-
examples/cssom_load rules 10	1429382270	1401409276	-	-
examples/cssom_load sheets 1	96730967	96807267	-	-
examples/cssom_load sheets 10	903977702	904498775	-	-
examples/dataset_load keys 1000 1	17711508	17729847	-	-
examples/dataset_load keys 1000 50	337491420	204313192	-	-
examples/dataset_load keys 10000 1	113438175	113706456	-	-
examples/dataset_load keys 10000 50	3206394774	1873481144	-	-
examples/dataset_load read 1000 1	14296468	14310424	-	-
examples/dataset_load read 1000 50	35190455	29948066	-	-
examples/dataset_load read 10000 1	78653696	78916714	-	-
examples/dataset_load read 10000 50	180462159	127828589	-	-
examples/dataset_load write 1000 1	18459080	17558253	-	-
examples/dataset_load write 1000 50	32273530	31339974	-	-
examples/dataset_load write 10000 1	120457419	111541947	-	-
examples/dataset_load write 10000 50	151664214	142116264	-	-
examples/date_load cap 1	55869674	55872509	-	-
examples/date_load cap 10	533948915	533951052	-	-
examples/date_load clock 1	21882639	21884827	-	-
examples/date_load clock 10	194994926	194997767	-	-
examples/date_load gc 1	32082117	32084352	-	-
examples/date_load gc 10	297074425	297076326	-	-
examples/date_load iso 1	1878253667	1878256018	-	-
examples/date_load iso 10	18648496704	18648499751	-	-
examples/date_load longstr 1	2563648	2566042	-	-
examples/date_load longstr 10	2563838	2566020	-	-
examples/date_load parse 1	232920323	232923587	-	-
examples/date_load parse 10	2238482111	2238484138	-	-
examples/date_load utc 1	176168462	176170668	-	-
examples/date_load utc 10	1763356337	1763358255	-	-
examples/eval_load calls 1	349788828	349789276	-	-
examples/eval_load calls 10	3476595376	3476595513	-	-
examples/eval_load closures 1	66764535	66764090	-	-
examples/eval_load closures 10	642113639	642113882	-	-
examples/eval_load concat 1	3184410	3184277	-	-
examples/eval_load concat 10	4939033	4939388	-	-
examples/eval_load fib 1	17620881	17621379	-	-
examples/eval_load fib 10	165320044	165319347	-	-
examples/eval_load loop 1	485924774	485924601	-	-
examples/eval_load loop 10	4923371517	4923371672	-	-
examples/eval_load objects 1	243024548	243025141	-	-
examples/eval_load objects 10	2371910148	2371910000	-	-
examples/eval_load throws 1	190038031	190037765	-	-
examples/eval_load throws 10	1888142771	1888143141	-	-
examples/eval_load webpack 1	6828831	6825884	-	-
examples/eval_load webpack 10	38824465	38800668	-	-
examples/fetch_load bytes 1	7890606	7918340	-	-
examples/fetch_load bytes 10	24708188	24725858	-	-
examples/fetch_load iterate 1	202875967	203078377	-	-
examples/fetch_load iterate 10	1971357270	1973229392	-	-
examples/fetch_load objects 1	86049244	86307340	-	-
examples/fetch_load objects 10	810015659	812401168	-	-
examples/fetch_load post 1	19000526	19025076	-	-
examples/fetch_load post 10	126617416	126679404	-	-
examples/focus_load dispatch 1	34673219	34708499	-	-
examples/focus_load dispatch 10	295900394	295607694	-	-
examples/focus_load focus 1	45744829	45831153	-	-
examples/focus_load focus 10	401825070	397139828	-	-
examples/focus_load gc 1	45954148	45884895	-	-
examples/focus_load gc 10	408749452	407433800	-	-
examples/fragment_load deep 1	32949242	32985489	-	-
examples/fragment_load deep 10	282976497	283212543	-	-
examples/fragment_load parse 1	50153635	50285022	-	-
examples/fragment_load parse 10	455003128	456074093	-	-
examples/fragment_load replace 1	25748148	25754693	-	-
examples/fragment_load replace 10	194735704	194759613	-	-
examples/gen_load delegate 1	23574456	23573868	-	-
examples/gen_load delegate 10	207911497	207911071	-	-
examples/gen_load finally 1	121233253	121232999	-	-
examples/gen_load finally 10	1181037071	1181036175	-	-
examples/gen_load forof 1	36084303	36085282	-	-
examples/gen_load forof 10	333984124	333983896	-	-
examples/gen_load iterobj 1	37279941	37279436	-	-
examples/gen_load iterobj 10	345673245	345673005	-	-
examples/gen_load next 1	60883298	60882299	-	-
examples/gen_load next 10	581407709	581407925	-	-
examples/gen_load spawn 1	101210316	101209967	-	-
examples/gen_load spawn 10	983393654	983396526	-	-
examples/gen_load spread 1	32869092	32869036	-	-
examples/gen_load spread 10	298601346	298601139	-	-
examples/gen_load suspended 1	19098390	19098196	-	-
examples/gen_load suspended 10	162888302	162888250	-	-
examples/geometry_load dashboard 1	13590731	13598175	-	-
examples/geometry_load dashboard 10	55574337	55594489	-	-
examples/geometry_load stretch 1	133270867	133277302	-	-
examples/geometry_load stretch 10	814094728	814152684	-	-
examples/handler_load 1	34264871	34244627	-	-
examples/handler_load 10	347923254	348244438	-	-
examples/interact_load asyncpriv 1	15320213	15320091	-	-
examples/interact_load asyncpriv 10	120262050	120261711	-	-
examples/interact_load churn 1	11470389	11470477	-	-
examples/interact_load churn 10	81797367	81797157	-	-
examples/interact_load fanpriv 1	13315033	13314786	-	-
examples/interact_load fanpriv 10	100035514	100034935	-	-
examples/interact_load supercalls 1	15592531	15591701	-	-
examples/interact_load supercalls 10	123130374	123129897	-	-
examples/interact_load suspended 1	11811106	11810726	-	-
examples/interact_load suspended 10	84981960	84982005	-	-
examples/intl_load cmp 1	56055664	56055505	-	-
examples/intl_load cmp 10	536224098	536223584	-	-
examples/intl_load cmp_opts 1	94767051	94767136	-	-
examples/intl_load cmp_opts 10	920697219	920696784	-	-
examples/intl_load format 1	60666778	60666615	-	-
examples/intl_load format 10	571243508	571243393	-	-
examples/intl_load locale 1	167134963	167121266	-	-
examples/intl_load locale 10	1635368134	1635354601	-	-
examples/intl_load make 1	11265942	11265978	-	-
examples/intl_load make 10	83513462	83512914	-	-
examples/intl_load nf_format 1	92756606	92756283	-	-
examples/intl_load nf_format 10	945829604	945829632	-	-
examples/intl_load nf_locale 1	118974741	119034748	-	-
examples/intl_load nf_locale 10	1207423778	1208024288	-	-
examples/intl_load nf_locale_opts 1	149913669	149985555	-	-
examples/intl_load nf_locale_opts 10	1470044259	1470776522	-	-
examples/intl_load parts 1	15837482	15837033	-	-
examples/intl_load parts 10	129459614	129459852	-	-
examples/intl_load plain 1	91608043	91607969	-	-
examples/intl_load plain 10	893488437	893488122	-	-
examples/iterator_load array_chain 1	88111427	88111523	-	-
examples/iterator_load array_chain 10	859567105	859567460	-	-
examples/iterator_load chain 1	69733641	69733502	-	-
examples/iterator_load chain 10	674834097	674834583	-	-
examples/iterator_load chain_forof 1	69987654	69987643	-	-
examples/iterator_load chain_forof 10	677316165	677315486	-	-
examples/iterator_load flatmap 1	155918463	155918695	-	-
examples/iterator_load flatmap 10	1537597343	1537596990	-	-
examples/iterator_load generic 1	55660962	55660797	-	-
examples/iterator_load generic 10	533211752	533211913	-	-
examples/iterator_load promise_new 1	35494827	35494180	-	-
examples/iterator_load promise_new 10	330420589	330420444	-	-
examples/iterator_load resolvers 1	37134268	37134217	-	-
examples/iterator_load resolvers 10	346904420	346904334	-	-
examples/iterator_load set_diff 1	40137651	40137811	-	-
examples/iterator_load set_diff 10	297368886	297369032	-	-
examples/iterator_load set_inter 1	41878592	41878490	-	-
examples/iterator_load set_inter 10	303045281	303045288	-	-
examples/iterator_load set_like 1	51909245	51909129	-	-
examples/iterator_load set_like 10	403076883	403076350	-	-
examples/iterator_load set_sym 1	42431124	42430993	-	-
examples/iterator_load set_sym 10	311922974	311923117	-	-
examples/iterator_load set_union 1	41740302	41740271	-	-
examples/iterator_load set_union 10	305144788	305144896	-	-
examples/iterator_load set_union_like 1	43409505	43409140	-	-
examples/iterator_load set_union_like 10	321573715	321573527	-	-
examples/js2_gen_load next 100000	170962972	168581710	-	-
examples/js2_lex_load small 1 1	114085516	115134148	120550427	121600295
examples/js2_parse_load full stmts 1	11011209	11074542	17739219	17807807
examples/js2_promise_load then 100000	151029221	151717929	-	-
examples/lex_load comments 1	42971606	42971566	-	-
examples/lex_load comments 10	425547661	425548149	-	-
examples/lex_load malformed 1	409492005	409492293	-	-
examples/lex_load malformed 10	4088546400	4088546152	-	-
examples/lex_load repeat 1	252324605	252324364	-	-
examples/lex_load repeat 10	2516983553	2516983343	-	-
examples/lex_load small 1	541244692	541244482	-	-
examples/lex_load small 10	5405918088	5405917686	-	-
examples/lex_load strings 1	85316463	85317035	-	-
examples/lex_load strings 10	853211346	853211482	-	-
examples/loader_load blocking 1	8608253	8636396	-	-
examples/loader_load blocking 10	28678039	28733032	-	-
examples/loader_load dynamic 1	11964763	12012320	-	-
examples/loader_load dynamic 10	63003225	63147007	-	-
examples/loader_load mixed 1	10468312	10507876	-	-
examples/loader_load mixed 10	46410124	46478240	-	-
examples/loader_load modules 1	9478461	9513037	-	-
examples/loader_load modules 10	26539758	26577487	-	-
examples/navigation_load frames 1	108639983	108378774	-	-
examples/navigation_load frames 10	1036442058	1033937153	-	-
examples/navigation_load history 1	69324160	69351737	-	-
examples/navigation_load history 10	644905494	645004931	-	-
examples/navigation_load iterate 1	251845367	251205905	-	-
examples/navigation_load iterate 10	2458759107	2452115610	-	-
examples/navigation_load route 1	52635903	52600546	-	-
examples/navigation_load route 10	480178000	479496845	-	-
examples/navigation_load storage 1	47202589	47375586	-	-
examples/navigation_load storage 10	421169558	422484148	-	-
examples/navigation_load subclass 1	63351385	63371862	-	-
examples/navigation_load subclass 10	574056901	574063427	-	-
examples/navigation_load url 1	120661291	120469693	-	-
examples/navigation_load url 10	1167626567	1165762267	-	-
examples/nodelist_load churn 1	49351435	49459766	-	-
examples/nodelist_load churn 10	426643772	427691925	-	-
examples/nodelist_load live 1	43438045	43300811	-	-
examples/nodelist_load live 10	342651887	341340171	-	-
examples/nodelist_load scan 1	57238466	57170556	-	-
examples/nodelist_load scan 10	178754050	177317274	-	-
examples/numeric_load atan2 1000 3528.2695431739785	11638687	11638433	-	-
examples/numeric_load atan2 10000 35649.31529475077	66943316	66943849	-	-
examples/numeric_load clz32 1000 23023	11157073	11156864	-	-
examples/numeric_load clz32 10000 196383	62295075	62294801	-	-
examples/numeric_load empty 1000 0	10139134	10138630	-	-
examples/numeric_load empty 10000 0	52538401	52537983	-	-
examples/numeric_load encodeuri 1000 14890	20127664	20127494	-	-
examples/numeric_load encodeuri 10000 158890	151405386	151404920	-	-
examples/numeric_load escape 1000 15890	19146263	19145994	-	-
examples/numeric_load escape 10000 168890	143911008	143910969	-	-
examples/numeric_load log 1000 5912.128178488171	11460322	11460395	-	-
examples/numeric_load log 10000 82108.92783681415	65191087	65191237	-	-
examples/numeric_load parsefloat 1000 71357.14285714286	19713518	19713328	-	-
examples/numeric_load parsefloat 10000 7142142.857142856	147183520	147183582	-	-
examples/numeric_load pow 1000 395012.24515318964	11383970	11383603	-	-
examples/numeric_load pow 10000 3950122.4515318926	65154859	65154955	-	-
examples/numeric_load radix16 1000 10550	14946214	14945994	-	-
examples/numeric_load radix16 10000 112922	102000947	102000793	-	-
examples/numeric_load radix2sub 1000 1075006	67955851	67956128	-	-
examples/numeric_load radix2sub 10000 10750005	629761526	629761077	-	-
examples/numeric_load radix36frac 1000 12029	14017339	14017384	-	-
examples/numeric_load radix36frac 10000 118679	92039188	92038840	-	-
examples/numeric_load random 1000 0	11267750	11271908	-	-
examples/numeric_load random 10000 0	63283205	63279106	-	-
examples/numeric_load sin 1000 999.9870900935418	11473307	11473563	-	-
examples/numeric_load sin 10000 10001.939505410717	65317469	65317305	-	-
examples/numeric_load toexp 1000 15916	19835717	19835217	-	-
examples/numeric_load toexp 10000 163574	151165771	151165214	-	-
examples/numeric_load tofixed 1000 5230	14539648	14539304	-	-
examples/numeric_load tofixed 10000 62230	97021947	97021929	-	-
examples/numeric_load toprecision 1000 7000	14900048	14899758	-	-
examples/numeric_load toprecision 10000 70000	99213464	99213647	-	-
examples/observer_load mo 1	81341790	82024562	88600399	89278091
examples/observer_load mo 10	1016049163	1025299712	1032453762	1041638010
examples/observer_load raf 1	146948962	147135079	-	-
examples/observer_load raf 10	1398776672	1401985749	-	-
examples/parse_load bodies 1	52838978	52736504	-	-
examples/parse_load bodies 10	521988497	520944898	-	-
examples/parse_load chain 1	113655031	113354949	-	-
examples/parse_load chain 10	1145154947	1142154784	-	-
examples/parse_load deep 1	FAIL	7250882	FAIL	14276004
examples/parse_load deep 10	FAIL	7456838	FAIL	14591518
examples/parse_load malformed 1	21865735	21824018	-	-
examples/parse_load malformed 10	211392883	210973963	-	-
examples/parse_load stmts 1	47487717	47397685	-	-
examples/parse_load stmts 10	463088089	462096542	-	-
examples/platform_load class 1	61684365	62033223	69103938	69462737
examples/platform_load class 10	507568750	510930472	522076639	525471537
examples/platform_load media 1	47175600	47131563	-	-
examples/platform_load media 10	400152712	399495007	-	-
examples/platform_load query 1	56387673	56521141	-	-
examples/platform_load query 10	269660411	270197851	-	-
examples/preload_load dups 1	5982340	6024795	12898102	12918789
examples/preload_load font 1	5982850	6018765	12976467	12979121
examples/preload_load script 1	6378305	6417384	13416350	13361409
examples/preload_load script 10	16181484	16252266	-	-
examples/preload_load style 1	6706172	6742180	13755851	13793298
examples/preload_load style 10	26436366	26521572	-	-
examples/promise_all_load all 1	25130009	10338370	-	-
examples/promise_all_load all 10	218495745	93933927	-	-
examples/promise_all_load any 1	20824297	8950944	-	-
examples/promise_all_load any 10	175688613	79419206	-	-
examples/promise_all_load anyerr 1	22923949	10524332	-	-
examples/promise_all_load anyerr 10	196301556	90144651	-	-
examples/promise_all_load many 1	16020607	8138499	-	-
examples/promise_all_load many 10	127575095	69029976	-	-
examples/promise_all_load race 1	19114418	8536297	-	-
examples/promise_all_load race 10	158680735	74844562	-	-
examples/promise_all_load settled 1	29243982	12826639	-	-
examples/promise_all_load settled 10	259317445	120803954	-	-
examples/promise_all_load thenable 1	26937471	10475435	-	-
examples/promise_all_load thenable 10	236235205	98994096	-	-
examples/promise_load chain 1	9408392	9408441	-	-
examples/promise_load chain 10	66214447	66214500	-	-
examples/promise_load closures 1	15280343	15280118	-	-
examples/promise_load closures 10	124640896	124641265	-	-
examples/promise_load ctor 1	17845647	17845361	-	-
examples/promise_load ctor 10	150410506	150410225	-	-
examples/promise_load fanout 1	9965265	9965079	-	-
examples/promise_load fanout 10	71685884	71685270	-	-
examples/promise_load finally 1	24381242	24380945	-	-
examples/promise_load finally 10	215191435	215191708	-	-
examples/promise_load reject 1	15574321	15574411	-	-
examples/promise_load reject 10	129089075	129089468	-	-
examples/promise_load thenable 1	17554980	17555297	-	-
examples/promise_load thenable 10	147506915	147507398	-	-
examples/proxy_load array_mapfilter 1	119183635	119183158	-	-
examples/proxy_load array_mapfilter 10	1116493234	1116493310	-	-
examples/proxy_load chain 1	31817767	31817250	-	-
examples/proxy_load chain 10	290829338	290829899	-	-
examples/proxy_load forward 1	13711513	13711949	-	-
examples/proxy_load forward 10	109715842	109715611	-	-
examples/proxy_load get_trap 1	7466028	7465790	-	-
examples/proxy_load get_trap 10	47723005	47722910	-	-
examples/proxy_load keys 1	54770863	54770232	-	-
examples/proxy_load keys 10	512270940	512270828	-	-
examples/proxy_load noop 1	10057348	10057239	-	-
examples/proxy_load noop 10	73696469	73696314	-	-
examples/proxy_load reactive 1	24122619	24122437	-	-
examples/proxy_load reactive 10	211405034	211404222	-	-
examples/proxy_load set_trap 1	9114292	9114578	-	-
examples/proxy_load set_trap 10	64014281	64014397	-	-
examples/range_load iter 1	39620829	39600790	-	-
examples/range_load iter 10	532688942	532381866	-	-
examples/range_load live 1	165883699	165876675	-	-
examples/range_load live 10	1422715649	1422601469	-	-
examples/recursion_load plain 100	2753066	2753132	-	-
examples/recursion_load plain 1000	4826278	4826505	-	-
examples/recursion_load react 100	5228480	5225639	-	-
examples/recursion_load react 1000	27377347	27376398	-	-
examples/recursion_load runaway 1	21219555	21219279	-	-
examples/reflect_load construct_args 1	10846653	10846607	-	-
examples/reflect_load construct_args 10	35557895	35557581	-	-
examples/reflect_load construct_class 1	20497168	20497400	-	-
examples/reflect_load construct_class 10	177162459	177161316	-	-
examples/reflect_load construct_fn 1	17583851	17583905	-	-
examples/reflect_load construct_fn 10	148524956	148524456	-	-
examples/reflect_load cross_script 1	17838899	17838718	-	-
examples/reflect_load cross_script 10	142279467	142278828	-	-
examples/reflect_load keys 1	19603098	19603038	-	-
examples/reflect_load keys 10	158664616	158662930	-	-
examples/reflect_load nested 1	13761980	13761937	-	-
examples/reflect_load nested 10	106221893	106221738	-	-
examples/reflect_load views 1	59932795	59932595	-	-
examples/reflect_load views 10	566906532	566906518	-	-
examples/resources_load dups 1	13134479	13144986	-	-
examples/resources_load dups 10	112365902	112376813	-	-
examples/resources_load large 1	2595396	2605568	-	-
examples/resources_load large 10	15566434	15572926	-	-
examples/resources_load small 1	7014319	7056998	14598369	14625293
examples/resources_load small 10	61663599	61898547	-	-
examples/revive_load cycle 1	12213759	12213739	-	-
examples/revive_load cycle 10	12213576	12213769	-	-
examples/revive_load deep 1	4051524	4051389	-	-
examples/revive_load deep 10	14231100	14231041	-	-
examples/revive_load gc 1	4808323	4808471	-	-
examples/revive_load gc 10	25151636	25151801	-	-
examples/revive_load holes 1	28621706	28621362	-	-
examples/revive_load holes 10	268783679	268784240	-	-
examples/revive_load shallow 1	60700149	60699865	-	-
examples/revive_load shallow 10	585691423	585691180	-	-
examples/sc_load churn 1	86945794	86945510	-	-
examples/sc_load churn 10	713831500	713830932	-	-
examples/sc_load custom_iter 1	9499630	9499566	-	-
examples/sc_load custom_iter 10	67396743	67396645	-	-
examples/sc_load foreach_grow 1	8951426	8952018	-	-
examples/sc_load foreach_grow 10	62437925	62438347	-	-
examples/sc_load forof 1	21834061	21833876	-	-
examples/sc_load forof 10	176437077	176437344	-	-
examples/sc_load map_num 1	11223075	11222794	-	-
examples/sc_load map_num 10	73547567	73547852	-	-
examples/sc_load map_obj 1	15480695	15480315	-	-
examples/sc_load map_obj 10	126760732	126760430	-	-
examples/sc_load map_str 1	21610996	21609842	-	-
examples/sc_load map_str 10	190853526	190852706	-	-
examples/sc_load next_loop 1	14046763	14046881	-	-
examples/sc_load next_loop 10	105421134	105421330	-	-
examples/sc_load set_dedupe 1	12869950	12869838	-	-
examples/sc_load set_dedupe 10	87914309	87914025	-	-
examples/sc_load symbol_for 1	51483197	51483012	-	-
examples/sc_load symbol_for 10	127074470	127074292	-	-
examples/sc_load symbol_keys 1	18399366	18399267	-	-
examples/sc_load symbol_keys 10	156731357	156731482	-	-
examples/sc_load toprim 1	9175599	9175944	-	-
examples/sc_load toprim 10	64849837	64849847	-	-
examples/scheduler_load messages 1	26530236	26457168	-	-
examples/scheduler_load messages 10	205172208	204209204	-	-
examples/scheduler_load mixed 1	47013220	46945535	-	-
examples/scheduler_load mixed 10	408938144	408120580	-	-
examples/scheduler_load timers 10	164045298	163884825	-	-
examples/scripts_load cross 1	61503371	61502659	-	-
examples/scripts_load cross 10	587502411	587502067	-	-
examples/scripts_load few 1	15781968	15756817	-	-
examples/scripts_load few 10	129877734	129639810	-	-
examples/scripts_load host 1	82267417	82267534	-	-
examples/scripts_load host 10	826500479	826498591	-	-
examples/scripts_load many 1	9819572	9812224	-	-
examples/scripts_load many 10	71957583	71890041	-	-
examples/scripts_load reject 1	24809098	24808853	-	-
examples/scripts_load reject 10	193519704	193519348	-	-
examples/scroll_load 1000 300	3412070081	3412070847	-	-
examples/scroll_load 10000 300	13086364984	13086419854	-	-
examples/seam_load concat_flag 1	11944865	11945101	-	-
examples/seam_load concat_flag 10	91567690	91567028	-	-
examples/seam_load forof_break 1	7900048	7900253	-	-
examples/seam_load forof_break 10	52026988	52027331	-	-
examples/seam_load from_map_entries 1	16848649	16848280	-	-
examples/seam_load from_map_entries 10	140601830	140601636	-	-
examples/seam_load from_set 1	17992335	17992959	-	-
examples/seam_load from_set 10	128203093	128202589	-	-
examples/seam_load iter_methods 1	26490501	26490122	-	-
examples/seam_load iter_methods 10	237613762	237613635	-	-
examples/seam_load species_map 1	12424154	12424505	-	-
examples/seam_load species_map 10	96800969	96801156	-	-
examples/seam_load string_hooks 1	30979261	30979762	-	-
examples/seam_load string_hooks 10	282300933	282300969	-	-
examples/selection_load long 1	196014135	196030161	-	-
examples/selection_load long 10	2542094821	2541361440	-	-
examples/selection_load short 1	44639315	44650680	-	-
examples/selection_load short 10	392814726	392268685	-	-
examples/session_load chain 1	5335929	5411906	12448214	12526109
examples/session_load chain 10	38223138	38340195	-	-
examples/session_load heavy 1	43123577	43199469	-	-
examples/session_load heavy 10	396286954	396398157	-	-
examples/sheets_load dynamic 1	9294771	9326025	-	-
examples/sheets_load dynamic 10	33326024	33351563	-	-
examples/sheets_load imports 1	8762862	8797265	-	-
examples/sheets_load imports 10	13081108	13119925	-	-
examples/sheets_load linked 1	9164421	9197537	-	-
examples/sheets_load linked 10	19406161	19471959	-	-
examples/stack_load 10	3275150	3275066	-	-
examples/stack_load 100	5844418	5844350	-	-
examples/sticky_load 100 100	66467664	66557670	-	-
examples/streams_load churn 1	25192797	25205027	-	-
examples/streams_load churn 10	190990822	191001549	-	-
examples/streams_load flow 1	204544468	204612759	-	-
examples/streams_load flow 10	1950571837	1951002580	-	-
examples/streams_load many 1	44910839	44937023	-	-
examples/streams_load many 10	385930986	386067648	-	-
examples/streams_load pending 1	75372526	75425301	-	-
examples/streams_load pending 10	691864568	692244416	-	-
examples/stringify_load callback 1	13830504	13828249	-	-
examples/stringify_load callback 10	115215735	115212857	-	-
examples/stringify_load dag 1	4583071909	4583069494	-	-
examples/stringify_load dag 10	4583071835	4583069323	-	-
examples/stringify_load deep 1	3476079	3474203	-	-
examples/stringify_load deep 10	18505356	18502922	-	-
examples/stringify_load escape 1	9021920	9019774	-	-
examples/stringify_load escape 10	67176068	67173546	-	-
examples/stringify_load gc 1	4916918	4914642	-	-
examples/stringify_load gc 10	25370843	25368277	-	-
examples/stringify_load shallow 1	38177141	38174581	-	-
examples/stringify_load shallow 10	360670026	360667829	-	-
examples/style_load attrs 1	33922419	33961087	-	-
examples/style_load attrs 10	247786627	248116508	-	-
examples/style_load churn 1	40993078	41090574	-	-
examples/style_load churn 10	302808702	303640452	-	-
examples/style_load setget 1	54037311	54013572	-	-
examples/style_load setget 10	394431250	395108683	-	-
examples/style_load shorthand 1	127774147	127739910	-	-
examples/style_load shorthand 10	1159730533	1160277065	-	-
examples/tagged_load call 1	36304892	36304830	-	-
examples/tagged_load call 10	336544826	336544697	-	-
examples/tagged_load css 1	199332383	199332675	-	-
examples/tagged_load css 10	1969372936	1969372788	-	-
examples/tagged_load plain 1	36212997	36212635	-	-
examples/tagged_load plain 10	335551089	335551161	-	-
examples/tagged_load sites 1	19220240	19190817	-	-
examples/tagged_load sites 10	166444830	166155241	-	-
examples/weak_load chain_fwd 1	23782142	23781969	-	-
examples/weak_load chain_fwd 10	201361892	201361850	-	-
examples/weak_load chain_rev 1	23414885	23415060	-	-
examples/weak_load chain_rev 10	197905752	197905525	-	-
examples/weak_load churn 1	40592666	40592512	-	-
examples/weak_load churn 10	370053908	370054313	-	-
examples/weak_load closures 1	24328882	24328781	-	-
examples/weak_load closures 10	210945510	210945535	-	-
examples/weak_load dead_cycles 1	10049596	10049543	-	-
examples/weak_load dead_cycles 10	69345537	69345298	-	-
examples/weak_load map_obj 1	16185257	16184702	-	-
examples/weak_load map_obj 10	130078024	130077575	-	-
examples/weak_load set_obj 1	16614567	16614779	-	-
examples/weak_load set_obj 10	134296968	134297215	-	-
examples/web_apis_load abort 1	87997738	88074582	-	-
examples/web_apis_load abort 10	815481417	816302227	-	-
examples/web_apis_load base64 1	523666381	523692344	-	-
examples/web_apis_load base64 10	5464067917	5464560048	-	-
examples/web_apis_load clone 1	141836853	141865940	-	-
examples/web_apis_load clone 10	1353552367	1353647278	-	-
examples/web_apis_load matchall 1	110418962	110432710	-	-
examples/web_apis_load matchall 10	1044771453	1044807427	-	-
examples/web_apis_load segmenter 1	122097519	122041009	-	-
examples/web_apis_load segmenter 10	1154226974	1153597952	-	-
examples/wrapper_load empty 1000 0	9911979	9911263	-	-
examples/wrapper_load empty 10000 0	50322133	50321305	-	-
examples/wrapper_load forin 1000 3000	21158124	21157415	-	-
examples/wrapper_load forin 10000 30000	161303737	161303097	-	-
examples/wrapper_load json 1000 3000	16212687	16212309	-	-
examples/wrapper_load json 10000 30000	112540009	112539544	-	-
examples/wrapper_load looseeq 1000 1000	12798106	12798273	-	-
examples/wrapper_load looseeq 10000 10000	78283898	78283882	-	-
examples/wrapper_load newbool 1000 500	12705860	12705836	-	-
examples/wrapper_load newbool 10000 5000	77825783	77825947	-	-
examples/wrapper_load newnumber 1000 499500	12231526	12230658	-	-
examples/wrapper_load newnumber 10000 49995000	72842597	72842415	-	-
examples/wrapper_load newstring 1000 3890	15839664	15839185	-	-
examples/wrapper_load newstring 10000 48890	109836680	109836183	-	-
examples/wrapper_load objectprim 1000 1000	11850431	11849905	-	-
examples/wrapper_load objectprim 10000 10000	69122224	69121637	-	-
examples/wrapper_load primcall 1000 5890	14660999	14660852	-	-
examples/wrapper_load primcall 10000 68890	96525644	96525052	-	-
examples/wrapper_load strindex 1000 1000	12975555	12975268	-	-
examples/wrapper_load strindex 10000 10000	80511842	80511299	-	-
examples/wrapper_load strkeys 1000 6000	22598968	22598475	-	-
examples/wrapper_load strkeys 10000 60000	176817748	176817397	-	-
examples/wrapper_load wrapcall 1000 4890	15575946	15576138	-	-
examples/wrapper_load wrapcall 10000 58890	105417139	105416571	-	-
examples/xhr_load 1	7547496	7573940	-	-
examples/xhr_load 100	14912960	14964098	-	-
```

</details>

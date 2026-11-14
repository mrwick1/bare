# Wrapping verification

Date: 2026-11-14. Issue: #467. Code, resource and scoped browser checks pass.

## Source and checks

The frozen production baseline is 8713d4c2. Its doc-only head is c57e06df. Candidate production source is 6841f650. Candidate bare-mcp SHA-256 is a3897b4915dec3debe9533a5e292592739b3a165a9775773623ffe20c398e14f.

The serial release command tests bare-css, bare-layout, bare-paint and bare with --lib and --test-threads=1. It passes 1,917 tests: 1,442 browser, 265 CSS, 161 layout and 49 paint. Seven tests remain ignored. CSS clippy completes with five existing warnings; layout adds no warnings. The new module format check and git diff check pass. No strict-clippy or full-workspace success is claimed.

The initial property, alias, inheritance, supports and memory tests failed before implementation. The long-word test failed at 20 pixels. The flex minimum test failed at 1,008 pixels. The grapheme helper test failed before the helper existed. JavaScript alias, preserved-newline capitalization and tab tests also failed before their fixes.

Checks now cover both wrap properties, the legacy alias, live computed text and relayout, no-wrap and pre, inline edges, spacing, case expansion, tiny boxes and clusters across DOM nodes. A 20,000-byte run keeps contiguous, non-overlapping source ranges. Flex and grid shrink with anywhere and break-all. Break-word retains the unbroken minimum.

## Resource method

The machine uses Linux x86-64 and a Ryzen 9 5900HX. Build jobs are limited to two. The host is shared. No wall-time comparison is claimed. Each interval below contains three fresh process samples. Instruction change compares minima. Memory samples omit the allocator tcache setting and use a C fork/exec/wait4 wrapper. RSS is in KiB. Raw samples and binary hashes remain outside the repository.

ComputedStyle grows from 856 to 872 bytes, as approved. A page with many unique styles shows the corresponding memory cost below. The pending Item fields are unchanged and measure 24 bytes. The old 20-byte comment was corrected. Layout uses the existing ICU segmenter 2.4.0 with compiled data; the lockfile has one version.

The first paragraph control grew by about 0.65 percent. Separate constant-generic normal and opted-in paths removed per-word wrapping work from normal pages. The final ordinary paragraph count is at baseline. Affected units use one joined UTF-8 segmentation pass per sizing or placement pass. There is no repeated suffix segmentation on each new line.

| Case | Baseline instructions | Candidate instructions | Change | Baseline RSS | Candidate RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| paragraph | 11,992,130–11,992,903 | 11,990,366–11,990,803 | -0.015% | 5,748–5,836 | 5,548–5,792 |
| identifier | 4,028,864–4,029,746 | 4,028,342–4,029,238 | -0.013% | 5,632–5,756 | 5,636–5,764 |
| spans | 309,163,119–309,163,524 | 308,881,347–308,882,539 | -0.091% | 13,088–13,124 | 13,088–13,196 |
| unique_styles | 996,112,593–996,113,391 | 989,607,480–989,607,688 | -0.653% | 24,864–25,216 | 25,500–25,548 |
| style_setget | 57,124,717–57,135,970 | 57,233,875–57,266,435 | +0.191% | 12,200–12,376 | 12,212–12,488 |
| browser_mutation | 70,843,022–70,844,551 | 70,983,114–70,984,017 | +0.198% | 11,352–11,768 | 11,492–11,620 |

The existing five-row cost gate passes the unchanged 0.5 percent limit under both allocators. Its glibc/jemalloc changes are: paragraph -0.014/+0.017 percent; long word -0.016/+0.027; spans -0.091/-0.088; browser mutation +0.197/+0.200; style set/get +0.171/+0.156. The full repository cost matrix was not run.

## Browser check and limits

The exact baseline and candidate binaries ran sequentially at 390 by 844. On the same synthetic page, the anywhere and break-all runs changed from 20 to 120 pixels high. The spaced control stayed at 160 pixels. Document width changed from 1,016 to 390 pixels. Both 120-character strings remained exact.

The unchanged Deck bundle used read-only API fixtures. Its 240-character request text changed from one clipped line to a 140-pixel-high wrapped block. The candidate kept the exact text inside a 302-pixel-wide area. The parent inspected both viewport captures. Supporting captures remain local; this document does not provide public image attachments.

The first diagnostic reads failed because getComputedStyle().font returned Unsupported. The worker filed #516, then changed the diagnostic to fontSize and fontFamily and reran. This did not follow the requested stop-on-error procedure. The rerun changed no application CSS or DOM. These results use the second-run captures and do not claim an uninterrupted browser pass.

The browser ran headless. A temporary screen existed, but the browser did not bind to it. No mirrored test is claimed. The owner declined a recording. Console logs were empty. The only failed request was the fixture's expected legacy bot 404. The challenge action was not clicked. The completion button was not visible in the captures and was not investigated. Colors from the separate #511 change were not tested here.

The source grapheme remains the mapping unit when case conversion expands it. Full language-specific line breaking, shaping, bidi and hyphenation are outside this change. No VM, account, credential, browser installation, merge or deployment is included. No recording was requested.

## Pre-merge integration

The branch was rebased onto main at 7c41b3e3, which includes the merged color fix. The conflicts joined adjacent color and wrap tests and retained both dependency notes. No wrapping algorithm changed. Fresh release tests and three-run measurements must pass on this combined source before merge.

Fresh samples use the same machine, inputs and three-run method stated above.

| Case | Baseline instructions | Candidate instructions | Baseline RSS KiB | Candidate RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| paragraph | 11,974,630–11,975,117 | 11,974,858–11,974,967 | 5,824–5,968 | 5,732–6,008 |
| identifier | 4,012,425–4,013,649 | 4,012,412–4,012,549 | 5,780–5,900 | 5,900–5,920 |
| spans | 307,509,679–307,510,131 | 307,109,404–307,109,836 | 13,220–13,332 | 13,196–13,368 |
| unique_styles | 993,795,525–993,836,645 | 991,060,977–991,061,322 | 24,968–25,256 | 25,480–25,540 |
| style_setget | 56,196,401–56,204,140 | 56,309,815–56,330,622 | 11,984–12,464 | 12,012–12,504 |
| browser_mutation | 69,951,203–69,953,001 | 70,100,903–70,101,907 | 11,476–11,556 | 11,368–11,812 |

All five selected cost rows pass the unchanged 0.5 percent threshold under both allocators. No row fails or exceeds the threshold under either allocator. Fresh pixel captures and the full cost matrix were not repeated for this rebase.

The rebased serial release run passes 1,954 tests: 1,456 browser, 288 CSS, 161 layout and 49 paint. Seven remain ignored. Main remained at 7c41b3e3 when the pre-merge checks completed.

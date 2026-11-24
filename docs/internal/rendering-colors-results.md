# Modern color verification

Date: 2026-11-14. Issue: #464. The documented checks pass for the tested scope.

## Source and method

The frozen baseline is 0afdae8038faa785f2dcb8f723268a7504e1adca. Its production source is 55caaa19. Candidate production source is 79164130. The candidate adds modern absolute colors, two-operand mixes, deferred currentColor evaluation and local-MINDE sRGB gamut mapping.

The measured machine uses Linux x86-64 and a Ryzen 9 5900HX. Build jobs are limited to two. The host is shared. Use instruction counts rather than wall time. Run each resource case three times. Preserve each sample and the binary hashes outside the repository.

Instruction samples use perf instructions:u and the existing tcache setting. Memory samples omit that setting. A small C fork/exec/wait4 wrapper reads child peak RSS. The first Python wrapper inherited the interpreter's high-water mark. Those memory samples are retained but excluded from comparison.

Cases: bare-bench css; style_load setget 1; style_load shown 1; sheets_load linked 1; browser_load mutation 1. The existing cost matrix also checks its matching browser rows under glibc and jemalloc. Keep its 0.5 percent limit unchanged.

## Storage and startup checks

Separate eager and lazy color arenas failed the short stylesheet cost gate. The selected storage shares one typed deferred arena with variable declarations. Side remains 72 bytes. Value remains eight bytes. Color remains four bytes. A stored deferred entry is 56 bytes; the previous variable declaration is 32 bytes. This shifts storage cost to declarations that need deferral.

The shared arena removed the legacy parser growth in an intermediate three-run check. The short browser regression remained. ELF inspection found a new shared math-library dependency and four new math imports. A separate build selected the color crate's Rust libm backend. The decision uses its measured result as well as the ELF inspection. The Rust libm feature removes the added shared-library dependency. The final three-row cost gate passes without a threshold change. The stylesheet row changes by +0.100 percent under glibc and +0.612 percent under jemalloc. The existing rule lists it as a one-allocator noise candidate; it does not exceed the limit under both allocators.

## Final resource samples

Each interval gives the minimum and maximum of three runs. Instruction change compares the minima. RSS uses KiB. These rows measure legacy workloads; they do not estimate the cost of newly supported syntax.

| Case | Baseline instructions | Candidate instructions | Change | Baseline RSS | Candidate RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| legacy_parse | 30,061,035,267–30,061,037,630 | 30,055,034,431–30,055,064,985 | -0.020% | 57,992–58,080 | 58,036–58,308 |
| style_setget | 57,304,854–57,316,624 | 57,300,160–57,304,838 | -0.008% | 12,180–12,388 | 12,300–12,496 |
| style_shown | 26,700,664–26,701,262 | 26,704,561–26,705,036 | +0.015% | 12,340–12,520 | 12,128–12,420 |
| sheets_linked | 9,384,245–9,391,308 | 9,384,508–9,385,515 | +0.003% | 12,180–12,664 | 12,252–12,440 |
| browser_mutation | 71,401,424–71,401,739 | 71,393,655–71,394,053 | -0.011% | 11,336–11,508 | 11,692–11,744 |

The cost-matrix browser mutation row changes by +0.000 percent under glibc and -0.038 percent under jemalloc. The style set/get row changes by +0.010 and -0.080 percent. The stylesheet row is stated above. No row exceeds 0.5 percent under both allocators. The full repository cost matrix was not run.

## Correctness checks

The first modern-color tests failed before implementation. The hint-prefix regression also failed before its fix: the inline picture variable resolved to none. It now retains its own arena entry.

Tests cover absolute colors, normalized weights, transparent operands, zero-sum weights, premultiplied alpha, nested float precision, missing components, hue direction, invalid syntax and 2,000 nested mixes. Gamut tests compare local-MINDE reference values within one byte. Cascade tests cover parent and element currentColor, theme variables, feature queries, cross-sheet indices, inline values and SVG hints. CSSOM tests cover shorthand serialization and distinct expressions with equal local indices.

The final serial release run passed 1,442 browser, 272 CSS, 150 layout and 49 paint tests. Seven tests were ignored. The total is 1,913 passed and seven ignored.

The default parallel browser run failed a download test with no available connection. The unchanged test uses a process-wide connection limit. The serial run passed. A debug run failed the unchanged meta-refresh test's ten-millisecond construction window. The debug layout test also overflowed its unchanged seed multiplication. These failures are recorded; they are not reported as green checks.

Default CSS clippy completes with five existing warnings. Strict clippy fails existing HTML type-complexity and CSS warnings. No unrelated lint cleanup is included.

## Pixel checks

The final binary passed the 390 by 844 synthetic page. Its SHA-256 is 5332805e6e72b923dc103b41164d69ba26c98c61219139e931cda8d1fb9bc107. The RGB control paints 255,0,0. The reported OKLCH paints 27,78,216. The Oklab red/blue mix paints 140,83,162. The inspected image contains all three swatches.

The real Deck bundle at 84820263 also passed. Its enabled Propose setup button paints 27,78,216 with white text. Its unchanged primary variable is oklch(48.8% .217 264). The element capture contains 3,446 primary-color pixels out of 4,160 pixels. The page uses read-only API fixtures. No setup is approved and no secret is entered.

These checks use headless bare through a private bridge. The created hyprcage screen cannot bind that headless session. No mirrored or headed test is claimed. Each worker stopped its server and candidate and destroyed its screen. The exact final binary passed both checks after the storage and math-backend changes. The parent also inspected both final viewport images. One refused POST in the fixture log came from the worker's startup probe; the app issued only GETs.

## Limits

This is not a full CSS Color conformance claim. Mixes require two operands and an explicit space. Relative colors, external profiles and math expressions in mix percentages remain unsupported. Resolved sRGB serialization remains unchanged.

No other viewport, dark scheme, hover or focus check has run. No VM, account, real credential, installed browser update, merge or deployment is included. Wrapping remains a separate change for #467.

## Pre-merge integration

The branch was rebased onto main at 8f23dded. Main added byte accounting for cached rule sets. The first rebased build failed because that accounting still named the replaced variable arena. The integration now counts the shared deferred allocation, variable strings, color-expression nodes and authored text. This preserves the cache's byte bound. Fresh release tests and repeated measurements must pass before merge.

Fresh samples use the same machine, inputs and three-run method stated above.

| Case | Baseline instructions | Candidate instructions | Baseline RSS KiB | Candidate RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| legacy_parse | 29,997,045,009–29,997,080,909 | 30,055,046,258–30,055,063,198 | 58,080–58,160 | 58,028–58,124 |
| style_setget | 56,204,160–56,224,024 | 56,203,572–56,224,480 | 12,104–12,280 | 12,204–12,380 |
| style_shown | 25,623,004–25,625,574 | 25,629,674–25,630,129 | 12,512–12,556 | 12,216–12,328 |
| sheets_linked | 8,582,808–8,582,931 | 8,583,685–8,584,451 | 12,412–12,608 | 12,544–12,656 |
| browser_mutation | 69,951,021–69,951,710 | 69,951,498–69,952,543 | 11,600–11,644 | 11,460–11,600 |

The three selected cost rows pass under both allocators at the unchanged 0.5 percent threshold. No row fails or exceeds the threshold under either allocator. The full matrix and fresh pixel captures were not repeated for this rebase.

The rebased serial release run passes 1,938 tests: 1,454 browser, 285 CSS, 150 layout and 49 paint. Seven remain ignored. The added heap check confirms that a deferred expression's counted allocation exceeds its authored text alone. The initial rebased compile failure and the corrected accounting are recorded above.

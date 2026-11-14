# Modern color verification

Date: 2026-11-14. Issue: #464. Final acceptance is pending.

## Source and method

The frozen baseline is 0afdae8038faa785f2dcb8f723268a7504e1adca. Its production source is 55caaa19. The candidate adds modern absolute colors, two-operand mixes, deferred currentColor evaluation and local-MINDE sRGB gamut mapping.

The measured machine uses Linux x86-64 and a Ryzen 9 5900HX. Build jobs are limited to two. The host is shared. Use instruction counts rather than wall time. Run each resource case three times. Preserve each sample and the binary hashes outside the repository.

Instruction samples use perf instructions:u and the existing tcache setting. Memory samples omit that setting. A small C fork/exec/wait4 wrapper reads child peak RSS. The first Python wrapper inherited the interpreter's high-water mark. Those memory samples are retained but excluded from comparison.

Cases: bare-bench css; style_load setget 1; style_load shown 1; sheets_load linked 1; browser_load mutation 1. The existing cost matrix also checks its matching browser rows under glibc and jemalloc. Keep its 0.5 percent limit unchanged.

## Storage and startup checks

Separate eager and lazy color arenas failed the short stylesheet cost gate. The selected storage shares one typed deferred arena with variable declarations. Side remains 72 bytes. Value remains eight bytes. Color remains four bytes. A stored deferred entry is 56 bytes; the previous variable declaration is 32 bytes. This shifts storage cost to declarations that need deferral.

The shared arena removed the legacy parser growth in an intermediate three-run check. The short browser regression remained. ELF inspection found a new shared math-library dependency and four new math imports. Test the color crate's Rust libm backend separately. Do not attribute the cost to the loader from inspection alone. Final backend measurements remain pending.

## Correctness checks

The first modern-color tests failed before implementation. The hint-prefix regression also failed before its fix: the inline picture variable resolved to none. It now retains its own arena entry.

Tests cover absolute colors, normalized weights, transparent operands, zero-sum weights, premultiplied alpha, nested float precision, missing components, hue direction, invalid syntax and 2,000 nested mixes. Gamut tests compare local-MINDE reference values within one byte. Cascade tests cover parent and element currentColor, theme variables, feature queries, cross-sheet indices, inline values and SVG hints. CSSOM tests cover shorthand serialization and distinct expressions with equal local indices.

An intermediate serial release run passed 1,442 browser, 272 CSS, 150 layout and 49 paint tests. Seven tests were ignored. The final math backend still needs the same run.

The default parallel browser run failed a download test with no available connection. The unchanged test uses a process-wide connection limit. The serial run passed. A debug run failed the unchanged meta-refresh test's ten-millisecond construction window. The debug layout test also overflowed its unchanged seed multiplication. These failures are recorded; they are not reported as green checks.

Default CSS clippy completes with five existing warnings. Strict clippy fails existing HTML type-complexity and CSS warnings. No unrelated lint cleanup is included.

## Pixel checks

The first candidate passed the 390 by 844 synthetic page. The RGB control paints 255,0,0. The reported OKLCH paints 27,78,216. The Oklab red/blue mix paints 140,83,162. The inspected image contains all three swatches.

The real Deck bundle at 84820263 also passed. Its enabled Propose setup button paints 27,78,216 with white text. Its unchanged primary variable is oklch(48.8% .217 264). The element capture contains 3,446 primary-color pixels out of 4,160 pixels. The page uses read-only API fixtures. No setup is approved and no secret is entered.

These checks use headless bare through a private bridge. The created hyprcage screen cannot bind that headless session. No mirrored or headed test is claimed. Each worker stopped its server and candidate and destroyed its screen. Final storage and math-backend changes still need an exact-binary pixel recheck.

## Limits

This is not a full CSS Color conformance claim. Mixes require two operands and an explicit space. Relative colors, external profiles and math expressions in mix percentages remain unsupported. Resolved sRGB serialization remains unchanged.

No other viewport, dark scheme, hover or focus check has run. No VM, account, real credential, installed browser update, merge or deployment is included. Wrapping remains a separate change for #467.

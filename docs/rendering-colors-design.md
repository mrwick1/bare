# Modern CSS colors in the existing painter

Date: 2026-11-14. Status: approved by the owner for implementation. Issue: #464.

## Verified baseline

The current color parser recognizes only rgb, rgba, hsl and hsla functions. It drops oklch and color-mix. A local 390 by 844 page confirms the failure. The RGB control paints red. The OKLCH and mix examples compute to transparent. The before image was inspected. The probe used the isolated browser candidate; the relevant CSS source matches this branch's base. It used no account or credential.

The real Deck bundle uses OKLCH theme variables, mixes in Oklab and Oklch, mixes containing currentColor, and Lab mix feature queries. Supporting only one literal color would leave valid theme rules disabled.

## Existing design and cost

Color is four bytes of RGBA. Most colors resolve during parsing. Custom properties already defer whole declarations for substitution. The cascade resolves currentColor after the element's text color. Side arenas already hold indexed calc and variable data. The painter accepts sRGB bytes.

Keep the four-byte paint color and the ordinary RGB, HSL, hex and named-color paths. Preserve the eight-byte CSS Value representation. Only a modern color expression that needs currentColor requires an indexed side entry. Conversion work belongs at parse or computed-value time, not on every paint.

## Options

1. Add handwritten conversion matrices and interpolation. This adds no dependency but duplicates color-science, missing-component and hue-interpolation rules.
2. Replace the complete CSS parser or renderer with a general styling stack. This is too broad for the failure.
3. Keep bare's tokenizer, cascade and painter. Use the pinned color crate for absolute modern-color parsing, color-space conversion and premultiplied interpolation. Add the CSS mix grammar and deferred currentColor binding in bare. Select this option.

The inspected color 0.3.3 crate is Apache-2.0 OR MIT. With its default std feature it needs no required third-party dependency. It supports the needed dynamic color spaces and CSS interpolation. Its clip operation is not a substitute for the CSS gamut-mapping algorithm. Use the CSS Color 4 reference mapping when reducing out-of-gamut results to sRGB.

Sources: [color API](https://docs.rs/color/0.3.3/color/), [CSS Color 4](https://www.w3.org/TR/css-color-4/), [CSS Color 5](https://www.w3.org/TR/css-color-5/).

## Proposed implementation

- Recognize modern absolute color functions through the existing value-parser entry point. Consume each complete function without reparsing the whole stylesheet.
- Parse color-mix operands, percentage defaults, normalization and alpha scaling. Support the standard interpolation spaces and hue directions represented by the selected library. Keep floating components through nested mixing. Quantize only the final result.
- Resolve constant expressions early. Store only expressions that depend on currentColor in a side arena. Resolve color-property currentColor from the parent and other properties from the element's computed text color.
- Extend every existing side-index relocation and variable-reparse path. An expression must not read another stylesheet's arena. Include expression identity in any declaration cache that retains unresolved values.
- Keep variable substitution in its existing phase. Support mixes containing substituted theme colors and transparent operands.
- Use an explicit bounded-by-input parse/evaluation stack for nested mixes. Avoid quadratic rescans or recursion proportional to hostile input depth.
- Preserve invalid-declaration recovery. Reject unsupported relative colors and external profiles explicitly. Do not claim complete CSS Color conformance or preservation of modern computed-value serialization; the existing resolved sRGB serialization remains.

## Verification gates

Before implementation, collect frozen baseline parser and browser-load instruction/memory rows. Run each chosen case three times. Use the same cases after the change. Report spread and host contention; do not infer speed from wall time on a busy host.

Write failing tests for the reported colors and a Tailwind-style variable theme. Cover nested mixes, currentColor on text and background, transparent operands, missing/explicit percentages, sums below and above 100 percent, hue interpolation, invalid syntax, and cross-sheet arena relocation. Use WPT or standards reference values, with an explicit numeric tolerance. Check gamut mapping rather than blindly clipping a reference result.

Run CSS, cascade and paint tests, then the browser library tests. Repeat the inspected 390-pixel page and a real Deck primary-button view. Check computed color and actual pixels. Retain before and after images at the same viewport. The button must be visible; no CSS fallback is allowed in Deck.

Record source, binary and dependency versions, resource rows, baseline failures and remaining unsupported syntax. Do not install the candidate, merge or deploy.

## Scope and stop conditions

Scope: color parsing and conversion, indexed expression storage, cascade resolution, their tests and necessary dependency/documentation changes. Do not refactor the painter, tokenization of unrelated values or application CSS.

Stop and revise this design if the chosen crate cannot preserve the required missing-component or interpolation semantics, or if the existing cache and side-index paths cannot safely carry deferred expressions. Performance and memory costs are not yet measured. No production code has changed.

## Implementation storage refinement

Measured on 2026-11-14. An eager color vector raised the legacy parser instruction count by about 0.83 percent. Use a lazy boxed vector header for deferred colors. Its empty state occupies one word. The extra allocation occurs only when a Side stores a currentColor-dependent expression. Color and Value keep their previous sizes. A three-run parser check reduced the instruction growth to about 0.67 percent. Final browser cost gates and limits belong in the results document.

The supported mix syntax has two operands and an explicit interpolation space. The latest CSS Color 5 draft also permits omitted spaces and more operands. These extensions are outside this change. Zero-sum weights follow the current draft: mix equally with zero alpha. Relative colors, external profiles and math expressions in mix percentages remain unsupported.

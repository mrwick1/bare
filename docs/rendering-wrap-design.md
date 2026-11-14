# Wrap unbroken text through the existing line breaker

Date: 2026-11-14. Status: proposed for owner review. Issue: #467.

## Verified baseline

The CSS property model has neither overflow-wrap nor word-break. The line breaker treats a word as an unbreakable pending unit. A local 200-pixel box with a 120-character run remains 20 pixels tall under both requested rules. A spaced-text control is 160 pixels tall. The 390-pixel before image shows horizontal overflow. The relevant layout source matches the frozen baseline used for that probe.

## Options

1. Split at every Unicode scalar. This is small but breaks combining sequences and emoji clusters.
2. Build a new paragraph shaping and line-breaking engine. This is too broad and changes the ordinary path.
3. Extend the current pending-unit breaker only for opted-in styles. Use the existing ICU version for grapheme boundaries and share the break policy with intrinsic sizing. Select this option.

Source: [CSS Text 3](https://www.w3.org/TR/css-text-3/). The JS crate already uses icu_segmenter 2.4.0 with compiled data. Add that same dependency configuration to layout instead of a second Unicode implementation. Keep its Unicode license notices. Do not introduce a second crate version.

## Proposed implementation

- Add inherited overflow-wrap values normal, anywhere and break-word, with word-wrap as an alias. Add inherited word-break values normal and break-all. Keep unrelated values unsupported rather than pretending they work.
- Add the two properties to the current core property model. Each Value is eight bytes, so the expected direct cost is 16 bytes per computed style. Measure the actual style and page-memory cost. Do not silently change the core-style size assertion. Packing into unrelated fields or a global side lookup would broaden the change and add risk.
- Read the policy once per text run. Preserve the existing ordinary word and whitespace path when both properties are normal, or when white-space forbids wrapping.
- For opted-in runs, identify grapheme boundaries once and retain source-byte mappings. Include boundaries across adjacent text nodes and inline spans. Segmenting each DOM node independently is insufficient for a combining or ZWJ sequence split across nodes.
- Reuse ICU over a joined unbreakable unit only on this path, with a mapping to the existing source pieces. Do not copy every normal paragraph or repeatedly segment the suffix on each new line. Work remains linear in the affected run, plus existing measurement.
- break-all adds ordinary opportunities between eligible grapheme units. overflow-wrap first respects ordinary word opportunities and uses emergency breaks when needed. Keep normal, anywhere and break-word semantics distinct.
- Share the effective break policy with min-content measurement. Anywhere and break-all affect intrinsic widths. Overflow-wrap break-word does not reduce min-content the same way. This matters for flex and grid overflow.
- Preserve spaces, newlines, byte offsets, painting, selection and hit testing. A cluster wider than the box may overflow; never drop it or loop without consuming input.

## Verification gates

Freeze the baseline before changes. Collect instruction and memory rows for ordinary paragraphs, long identifiers and many styled nodes. Run the same cases three times before and after. Report spread. Do not claim wall-time gains under host contention. Compare the expected style-size increase with actual page RSS and allocations.

Write failing CSS parsing, inheritance, alias, supports and computed-style tests. Add layout tests at the reported width for both rules and a normal control. Cover nowrap and pre behavior, combining marks, ZWJ emoji, clusters crossing DOM nodes, tiny boxes, nested styles and a very long run. Assert that fragments preserve every source byte. Add min-content, flex and grid cases, including the anywhere versus break-word distinction.

Run CSS and layout suites, then the browser library tests. Repeat the 390-pixel probe and the real Deck request-text view. Inspect actual pixels, line count and scroll width. Retain matching before and after images. No inserted spaces or CSS workaround is allowed in Deck.

## Scope and limits

Change CSS property/cascade handling, text segmentation for the opted-in path, inline placement, intrinsic sizing, related tests and the existing ICU dependency wiring. Do not rewrite the font system, paint pipeline or ordinary paragraph layout. Full language-specific line breaking, hyphenation and keep-all are outside this fix.

Stop and revise this design if cluster boundaries or source mappings cannot be preserved across inline nodes without a wider layout rewrite. The dependency and style-size costs need measurement. The owner must review this design before implementation. No production code has changed.

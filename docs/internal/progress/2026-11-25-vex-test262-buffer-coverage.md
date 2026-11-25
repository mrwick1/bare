# Enable buffer detachment and typed-array Proxy tests in Test262

The Vex runner captures the buffer detachment getter and fixed-length transfer before the test runs. The host hook transfers to length zero and returns undefined. A repeated call on a detached buffer has no effect.

The scoped Proxy rule applies to TypedArrayConstructors. A probe of the broader allowlist recovered 114 cases but also exposed 30 failures, so the rule leaves other directories unchanged.

The pinned buffer, typed-array, and array iterator groups at `9dc4a60d` passed 2,498 cases and skipped 501. Both candidate verifier modes pass 2,770 cases, fail none, and skip 229. All prior passes remain. Detachment accounts for 266 new passes and the Proxy rule accounts for six. Fifteen detachment cases still need Float16Array, immutable buffers, or the cross-realm host hook.

The runner type check passes. A host probe checks repeated detachment, a resizable buffer, view invalidation, an invalid receiver, and replacement of the captured built-ins. The change affects the Test262 runner only. Coverage excludes full Test262, browser journeys, and Windows.

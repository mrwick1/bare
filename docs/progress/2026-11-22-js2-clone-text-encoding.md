# 2026-11-22 — Provide UTF-8 text codecs in js2

Both Node.js and crates.io rejected promises because js2 lacked TextEncoder. Both bundles also use TextDecoder. js2 now installs both codecs on demand. The encoder replaces lone surrogates and counts UTF-16 units in encodeInto. The decoder keeps unfinished bytes across streaming calls and implements BOM and fatal options. The browser prints the name and message of a js2 Error rejection without a stack. It preserves the display of plain object reasons.

## Public journeys

The base code is `862ab0c3`. Fixed runs include the structured-clone and typed-array host lookup changes. Both engines use the same release driver, network recordings and virtual clock on archMachine. Instruction counts use perf, the large glibc thread cache and three passing runs. The minimum and range are in millions. Peak RSS uses wait4, the same allocator setting and three separate runs. The median and range are in KiB.

| crates.io | OLD | js2 | Replies |
|---|---:|---:|---:|
| Base instructions | 75.815 [75.815–76.247] | 85.591 [85.591–85.902] | 39 |
| Fixed instructions | 76.154 [76.154–76.309] | 143.851 [143.851–144.260] | 39 |
| Base peak RSS | 23,472 [23,040–23,628] | 23,568 [22,896–23,664] | 39 |
| Fixed peak RSS | 23,064 [22,980–23,104] | 25,076 [24,952–25,720] | 39 |

Fixed crates.io js2 uses 1.89× the OLD instructions. The codec change alone also passes three recorded crates.io runs in each engine with 39 replies. The fixed js2 runs have no console errors, including three live runs with 39 replies. OLD still reports its separate TypeError about reading slice on undefined. Fixed js2 peak RSS exceeds the spread of the base runs, which stopped script work at the codec errors. In three fixed live runs per engine, js2 used 281.531 [281.531–282.846] M instructions and OLD used 87.703 [87.703–87.849] M. The live ratio is 3.21×. Live peak RSS was 31,312 [31,148–31,440] KiB on js2 and 24,292 [23,980–24,472] KiB on OLD.

React Docs, GitHub Docs and Rust Book each pass three recorded runs in both modes with equal reply counts and no console errors. Compared with the same build before the codec change, each new-engine instruction minimum stays within its previous spread or falls. The updated js2 to OLD instruction ratios are 0.78×, 0.95× and 0.76×.

### Node.js remaining gap

The codec rejection no longer occurs. Node.js now reaches its locale code. All three recorded js2 runs stop on `Intl is not defined` after 22 replies. All three OLD runs pass with 23 replies. Two of the three fixed live js2 runs pass with 23 replies. The other live run reports the Intl error. There is no three-run passing matched set for a fixed ratio. The two passing live js2 runs used 1,018.327–1,029.469 M instructions and 44,808–46,340 KiB peak RSS.

An instrumented OLD replay observed `Intl.Locale("en")`, `Intl.NumberFormat.supportedLocalesOf("en")` and a NumberFormat for minutes with zero maximum fraction digits. Other formatter constructors are read but are not invoked in that replay. Share the locale and number formatting backend before adding the bindings in js2. This requires a larger cross-crate change than the page-fix brief permits.

## Checks and cost

The three codec tests failed on the missing globals and pass with the fix. The rejection smoke test prints `TypeError: codec test` for an Error and keeps the plain object display. The browser rejection tests and typecheck pass. Test262 has no TextEncoder or TextDecoder tests.

The `binary_load codec` and `stream` rows changed by at most 0.008% under glibc and jemalloc. All five `interp_load` rows changed by less than 0.001%. No row exceeded the cost limit. Raw data is in `/data/bare-tmp/r22/js2-clone`.

Local checks omit the full workspace suite, Windows, Miri and non-UTF-8 encodings.

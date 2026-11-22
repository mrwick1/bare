# 2026-11-22 — Pack shared Intl text tables

Pack the source strings at compile time. Each table stores UTF-8 bytes, 16-bit ranges and any scalar fields. A formatter reads only the selected strings. Keep the source tables and public constants used by the generators and callers. Use three-byte keys for currency symbols and digit counts. Precompute the five ICU locales. Relative time uses the cardinal provider so a binary without PluralRules can omit ordinal rules.

## Relocation inventory

Build on archMachine with release settings and debug symbols. Compare the standalone `script_run` from main `60188a32` with the original bindings at `602cb155`. The relative relocation count increases from 4,164 to 7,477. The `readelf -r --wide` output goes from 4,262 to 7,575 lines. The `.data.rel.ro` section grows from 83,352 to 154,528 bytes.

Use `nm -S --size-sort` to locate the named tables. For anonymous tables, match `readelf -r --wide` sections in the shared crate's object files to their first strings or the reader's references. The object counts precede linker deduplication and dead-code removal.

| Table | Pointer relocations in object data |
| --- | ---: |
| Currency names | 486 |
| Relative-time patterns | 306 |
| Unit suffixes and denominators | 270 + 135 |
| Display names, region / language / currency / script / field / dialect / calendar | 182 / 172 / 84 / 54 / 42 / 38 / 36 |
| Date names | 177 |
| Narrow and regular currency symbols, locale overrides and currency digits | 174 + 46 + 12 + 36 |
| Likely locale subtags | 120 |
| Time zone entries | 94 plus the alias slices |
| ICU cardinal and ordinal plural rules | 92 + 59 |
| ICU decimal symbols and collation reordering | 55 + 51 |

The first packed build drops 2,566 relative relocations from the standalone bindings. It still adds 747 against main. The minimum rises 1.309% under glibc and 0.238% under jemalloc. In that build four OLD Intl cost rows also exceed 0.5%. The second pass uses fixed-width currency keys, keeps the small UTC alias search, and avoids linking the unused ordinal provider in the standalone runner.

## Default startup

Use main `64ca5d47` for both final comparisons. The executable for the empty browser page is `bare-drive`. Its relative relocations fall from 38,305 to 35,788. The `readelf` line count falls from 38,502 to 35,985. The `.data.rel.ro` section falls from 888,728 to 837,440 bytes. The standalone js2 runner has 4,164 relative relocations in both builds because it does not yet link the shared crate.

Run `script_run --eval empty.js 0`, `eval_check -e 0`, and the empty loopback page in both browser modes. Use the minimum of three user-mode instruction counts and the median of three wait4 peak RSS measurements. Each cell shows the minimum or median and the min–max range. RSS is in KiB.

| Case | Allocator | Main instructions | Packed instructions | Main RSS | Packed RSS |
| --- | --- | ---: | ---: | ---: | ---: |
| Standalone js2 | glibc | 714,675 [714,675–715,505] | 715,334 [715,334–716,267] | 5,184 [5,076–5,336] | 5,084 [5,068–5,212] |
| Standalone js2 | jemalloc | 4,055,155 [4,055,155–4,055,879] | 4,055,618 [4,055,618–4,056,364] | 7,868 [7,652–7,912] | 8,028 [7,932–8,044] |
| Standalone OLD | glibc | 2,083,812 [2,083,812–2,084,956] | 2,053,719 [2,053,719–2,054,238] | 5,748 [5,660–5,888] | 5,448 [5,416–5,624] |
| Standalone OLD | jemalloc | 5,867,169 [5,867,169–5,868,809] | 5,836,540 [5,836,540–5,838,084] | 8,640 [8,376–8,792] | 8,440 [8,372–8,560] |
| Browser OLD | glibc | 5,465,384 [5,465,384–5,466,229] | 5,434,526 [5,434,526–5,472,686] | 18,092 [17,380–18,228] | 17,224 [17,016–17,548] |
| Browser OLD | jemalloc | 9,474,450 [9,474,450–9,486,260] | 9,445,075 [9,445,075–9,445,375] | 20,712 [20,516–20,732] | 20,400 [19,900–20,604] |
| Browser js2 | glibc | 4,150,418 [4,150,418–4,152,223] | 4,123,469 [4,123,469–4,124,236] | 17,136 [17,132–17,524] | 16,832 [16,800–16,864] |
| Browser js2 | jemalloc | 8,319,519 [8,319,519–8,321,635] | 8,291,211 [8,291,211–8,294,240] | 20,380 [20,252–20,452] | 19,968 [19,708–20,096] |

## Validation

The 43 OLD Intl tests, six OLD ICU tests and the packed locale test pass. All 3,393 OLD `intl402` outcomes match. Both runners pass 807 and fail 2,586. The `bare-js2` and `bare` typecheck passes.

All 20 OLD Intl cost rows meet the +0.5% instruction limit with both allocators. The largest increases are +0.026% under glibc and +0.044% under jemalloc. Mean changes are −0.346% and −0.342%.

The logs and recordings are under `/data/bare-tmp/r22/js2-intl-reloc` on archMachine. Local checks omit Windows, Miri and the full workspace suite.

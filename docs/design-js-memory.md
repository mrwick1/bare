# Design: peak memory of `python-docs-search-results` (criterion 6)

Task R2-C of perf goal round 2. This document changes no product code. The owner reviews it before anything is built.

Criterion 6 asks for a peak RSS of 92.8 MiB or lower (20% under the 116 MiB baseline). PR #501 brought the journey to 90 MiB. The final benchmark measured 99.3 MiB at `8f23dded`. This document says which merges added memory, what the bytes are, and which designs remove them.

## 1. Summary

- The JS heap is 47 MiB of the 81.9 MiB of live allocations at the peak (massif). The DOM arena is 22 MiB. All of that DOM comes from `DOMParser` documents, not from the page itself.
- One merge explains the growth: PR #497 (string atoms and ropes). It adds **5.1 to 5.5 MiB of live allocations** and **+7.3 MiB peak RSS** (median of 4 runs: 90.2 to 97.5 MiB). It stops interning strings of more than 32 units. The page fetches the same documents several times (Sphinx summary code). Each `text()` result is now a new copy. Before #497, equal texts were one string.
- PR #512 (compact objects), PR #515 (rule-set cache) and the other merges after #497 add 0.0 to 0.5 MiB of live allocations. The peak RSS moves by 1 to 2 MiB between them. The live allocations do not move. The move comes from the allocator state (section 4.3; hypothesis, not proven by a direct measure of the allocator). This is the "cause not found" of the final benchmark.
- The largest lever is not a data layout. glibc raises its mmap threshold when a large block is freed. A page that frees a 1.1 MB string then gets all later blocks up to 1.1 MB from the brk heap. That memory is not returned. A fixed threshold (`mallopt(M_MMAP_THRESHOLD, 256 KiB)`) gives **-6.0 MiB** with no code change in the engine. CPU time is higher by 0.4% to 6% on the journeys I measured (page faults; loaded machine).
- A shared string for equal `text()` results gives **-3.5 MiB** and -0.6% instructions. Together with the fixed threshold the effect is larger than the sum: **97.5 to 85.9 MiB (-11.6 MiB)**. The target is 92.8 MiB. The margin is 6.9 MiB.
- Design 2 (smaller layouts for arrays and atoms) reaches about 88 MiB without an allocator setting. It is the portable option. It is a hypothesis: nothing of it is built.
- A collect on memory pressure is not a good design today: one collect costs about 137 M instructions (section 6.4).

Recommendation: Design 1 (section 6.1), then measure CPU on an idle machine. Design 2 follows only if the owner wants more margin or a result that does not depend on glibc.

## 2. How this was measured

- Machine: archMachine (AMD Ryzen 9 5900HX, 16 cores). The machine was shared with other agents. Instruction counts and RSS hold under load. CPU time is weak.
- Journey: `python-docs-search-results`, recorded corpus, virtual clock (`BARE_CLOCK=virtual`), `BARE_SEED=1`. Every run had 0 replay misses.
- Bisect: `base-build <merge commit> --release -p bare-drive` for each merge. `tools/bench/journeys.sh run --clock virtual --runs 3 --pss` gives 4 rows per commit: run 0 (with the PSS sampler) and runs 1 to 3. The tables use all 4 rows. The commits are the first-parent merges from #498 to `28a8fbc2` (newest main). #511 and #517 are merged between #519 and #520. I did not build them.
- Counts of live data come from a second set of builds with a temporary patch. At the end of the journey, before the page is dropped, it prints `VmRSS`, `VmHWM`, `mallinfo2`, interpreter and heap footprint, a census of strings, and the DOM arena sizes. It can also run a full collect and print again. The patch is not in the repository (`/data/bare-tmp/r2/c/patch.py` and `exp-main-28a8fbc2.patch`; the second holds the experiments of section 6). The patch builds peak 0.4 to 2.8 MiB higher than the clean builds (main: 100.0 to 101.0 MiB against 98.2). I use them only for counts, never for RSS.
- Allocation sites come from `valgrind --tool=massif` on main `28a8fbc2` (build with line tables). `heaptrack` is not installed. I installed no package. `libjemalloc.so.2` was already installed; I used it only for one comparison.
- Peak RSS is reached at the end of the journey: `VmHWM` equals `VmRSS` when the patch prints. The drop of the page then frees 27 MiB (RSS 99.3 to 72.2 MiB on main).

## 3. The journey, in one paragraph

`docs.python.org/3/`: type "asyncio" in the search box. The page loads `searchindex.js` (879,079 bytes on the wire, one `Search.setIndex({...})` data literal). The search runs over the index. For each of the 20 or more results, Sphinx `searchtools.js` does `fetch(url).then(r => r.text())`, then `new DOMParser().parseFromString(html)`, removes `script`, `style` and `.headerlink`, reads `textContent` of `[role=main]`, and lower-cases it. Several results point to the same page. The network log shows `license.html` fetched twice, `whatsnew/3.10` to `3.13` twice each, and `whatsnew/3.14` four times. So the same text is made several times. The largest text in the census has 554,836 UTF-16 units and exists three times.

## 4. Result of the bisect

### 4.1 Peak RSS by merge

Clean `-p bare-drive` builds. The change is the change of the median against the row above.

| Merge | Peak RSS of 4 runs (MiB) | Median | Change | PSS, run 0 | Minstr |
| --- | --- | --- | --- | --- | --- |
| #498 lazy CSS declarations | 114.1, 113.4, 113.0, 114.2 | 113.7 |  | 110.4 | 3374 |
| #492 lazy JS parse | 112.5, 114.0, 114.2, 113.5 | 113.8 | +0.0 | 110.4 | 3376 |
| #494 ancestor buckets | 114.5, 113.7, 113.2, 115.3 | 114.1 | +0.4 | 108.6 | 3378 |
| **#501 compact data literals** | 90.4, 90.4, 89.7, 90.1 | 90.3 | -23.9 | 87.0 | 2541 |
| #502 share rule sets | 90.6, 90.7, 93.2, 90.9 | 90.8 | +0.5 | 89.2 | 2541 |
| #504 compression stream | 90.2, 90.2, 90.2, 90.5 | 90.2 | -0.6 | 87.8 | 2541 |
| **#497 atoms and ropes** | 97.0, 97.4, 98.0, 97.6 | 97.5 | +7.3 | 92.9 | 2558 |
| #507 | 95.6, 95.7, 95.3, 94.5 | 95.5 | -2.0 | 93.2 | 2558 |
| #506 gif images | 95.2, 96.0, 96.9, 96.4 | 96.2 | +0.8 | 93.7 | 2558 |
| #509 VM spike | 97.0, 97.8, 97.9, 96.6 | 97.4 | +1.2 | 90.4 | 2558 |
| #510 | 97.2, 96.2, 95.4, 94.8 | 95.8 | -1.6 | 95.7 | 2558 |
| #512 compact objects | 95.9, 96.6, 95.1, 95.6 | 95.8 | +0.0 | 92.7 | 2515 |
| #514 | 95.5, 96.1, 95.9, 96.0 | 95.9 | +0.1 | 89.0 | 2516 |
| #515 rule-set cache | 97.7, 96.4, 98.9, 97.8 | 97.8 | +1.8 | 91.2 | 2485 |
| #518 closures | 97.2, 96.9, 97.5, 97.2 | 97.2 | -0.6 | 94.6 | 2485 |
| #519 bound rule cache | 97.5, 96.7, 97.2, 96.5 | 96.9 | -0.2 | 95.2 | 2485 |
| #520 final benchmark | 97.8, 98.3, 97.7, 99.3 | 98.0 | +1.1 | 94.9 | 2487 |
| main `28a8fbc2` | 98.0, 97.1, 98.7, 98.3 | 98.2 | +0.1 | 90.5 | 2487 |

The spread of the 4 runs is 0.6 to 2.6 MiB. A step under about 2.5 MiB in this table is not proven. PSS comes from one run with a sampler and I do not use it for conclusions. The empty-page floor (`example-page`) is 15 to 16 MiB and did not move across the commits I checked.

### 4.2 Live allocations by merge (the stable number)

RSS wobbles. The bytes that `mallinfo2` reports as in use at the end of the journey change little between runs. The values are from the patch builds, 3 runs each (min to max).

| Merge | In use at the end (MiB) | String units (MiB) | Object slab (MiB) | Syntax trees with source (MiB) | Atoms (units); flat strings (units) |
| --- | --- | --- | --- | --- | --- |
| #494 | 104.3 to 104.9 | 10.95 | 11.74 | 19.40 (caches 5.73) | 85,029 (5.74 M); none |
| #501 | 79.2 to 79.6 | 10.95 | 11.74 | 3.94 (caches 0.09) | 85,012 (5.74 M); none |
| #504 | 78.6 to 79.5 | 10.95 | 11.74 | 3.94 | 85,012 (5.74 M); none |
| **#497** | **84.3 to 84.7** | **16.46** | 11.74 | 3.94 | 81,711 (1.40 M); 4,102 (7.21 M) |
| #507, #506, #509, #510 | 84.3 to 85.4 | 16.46 | 11.74 | 3.94 | same |
| #512 | 84.68 to 84.69 | 16.46 | 14.74 | 3.94 | same |
| #514 | 84.68 to 84.81 | 16.46 | 14.74 | 3.94 | same |
| #515, #518 | 85.15 to 85.29 | 16.46 | 14.74 | 3.94 | same |
| #519, #520, main | 84.47 to 84.85 | 16.46 | 14.74 | 3.94 | 81,723 (1.40 M); 4,102 (7.21 M) |

#502 is missing: its patch build printed at another moment (the numbers do not compare).

### 4.3 What each merge added

| Merge | Change in live bytes | Data structure and bytes |
| --- | --- | --- |
| #501 (compact literals) | -25 MiB | The search index is no longer a syntax tree. Syntax trees with source: 19.40 to 3.94 MiB. Their caches: 5.73 to 0.09 MiB. This gain made criterion 6 pass. |
| **#497 (atoms, ropes)** | **+5.1 to +5.5 MiB** | A string of more than 32 units is a new `Str` with its own `Box<[u16]>` each time (`ATOM_MAX`). Before #497 every string was an atom: 85,012 atoms, 5.74 M units, and an equal text was one string. After: 81,711 atoms (1.40 M units) and 4,102 flat strings (7.21 M units). Total units go from 5.74 M to 8.61 M (+2.87 M units, +5.5 MiB at 2 B). The repeated page texts of section 3 are now separate copies. After a full collect only 0.15 M flat units stay live (147,982 units), so this is garbage that the collector has not freed yet: it ran once (section 6.4). |
| #512 (compact objects) | +0.0 MiB live. The object slab goes from 11.74 to 14.74 MiB | The record goes from 80 to 104 B (+24 B for 96,559 objects: +2.3 MiB). About 88.7 K objects with one property lose a separate slot `Vec` (a malloc chunk each; about 2.8 MiB). The two cancel. |
| #515 (rule-set cache) | +0.5 MiB | Not examined in detail. |
| #509, #506, #510, #514, #518, #519, #520 | 0.0 to 0.2 MiB | Nothing visible. The RSS moves of 1 to 2 MiB between them match no change of live bytes. I did not find the cause. Hypothesis: allocator layout. Free memory that glibc keeps in its arenas after a forced collect is 11.9 to 25.4 MiB on main and changes from run to run. |

Peak RSS minus live bytes is 12.2 to 15.2 MiB on these commits. The process floor (`example-page`) is 15 to 16 MiB. The rest is glibc state and capacity that nothing touched (for example, the DOM node `Vec` has 13.0 MiB of capacity and 6.7 MiB used).

## 5. Where the memory is at the peak

### 5.1 Live allocations by part (massif peak, main, 81.9 MiB)

| Part | MiB | Share | What it is |
| --- | --- | --- | --- |
| DOM arena from `DOMParser` documents | 22.1 | 27% | Node `Vec` 13.0 + 1.36 (growth); text buffer 5.61; `shrink_to_fit` 1.0; UTF-8 copy in `string_arg` 1.06 |
| JS strings: fetched pages, text contents, lower-cased copies | 14.0 | 17% | Almost all garbage: a forced collect frees 13.5 MiB |
| JS object records (the slab) | 12.8 | 16% | 11.5 + 1.34 (growth) |
| JS atoms | 8.1 | 10% | 6.54 + 0.91 + 0.66 |
| JS array elements | 7.1 | 9% | 4.43 + 2.65 |
| JS dictionary properties | 5.0 | 6% | `PropMap` entries 3.04, hash index 1.94 |
| Page source text, copied in the syntax tree | 3.69 | 5% | the `searchindex.js` source |
| Network: bodies kept by the client | 3.75 | 5% | `Client::keep` |
| Other | about 5.4 | 7% | each part under 0.3% |

massif adds 8.2 MiB of allocator headers on top (about 1.07 million blocks, 8 B each). The JS heap is 47.1 MiB of the 81.9 (57%): strings 14.0 + data objects 33.0. Style and layout of the page itself are in "other": the page is small. About 14.7 MiB of the RSS is outside malloc (the process floor).

### 5.2 JS heap by kind (`Heap::footprint` and the census, main, end of journey)

| Kind | Count | MiB | Note |
| --- | --- | --- | --- |
| Object records (slab) | 96,559 objects (88,650 arrays; 96,491 with inline values) | 14.74 capacity; 9.6 used | 104 B record + 8 B `Meta` + 4 B `back` + 1 mark byte |
| Array elements | 468,846 elements | 7.15 | 16 B each (`Option<Value>`), one `Vec` per array |
| Dictionary properties | 102 objects | 2.72 used (3.10 capacity) + 1.66 index | 48 B per entry |
| Property slots in shape mode | | 1.45 | |
| Shapes | 1,953 | 0.20 | |
| Atoms | 81,723 (1.40 M units) | 2.7 for the units | `Str` 24 B + 13 B slab bookkeeping + a `Box<[u16]>` block each |
| Flat strings | 4,102 (7.21 M units) | 13.7 for the units | 342 wide strings hold 6.71 M units |
| String slab (all strings) | 86,139 | 4.74 | |
| Interner | | 1.00 | |
| Garbage when the journey ends | 5,433 objects, 570 strings | accounted 41.27 before the collect, 27.14 after (-14.1) | 1 collect ran during the journey |

Arrays by length: 1 element 20,288; 2: 13,611; 3: 27,791; 4 to 7: 22,125; 8 to 63: 3,881; 64 or more: 933 (150,615 elements). Arrays by kind of element: all `int32` 16,303 arrays (239,779 elements); strings only 62 (1,798); objects only 23,261 (51,103); mixed or holes 49,003 (176,166).

Latin-1 test (every unit under 256): atoms 81,264 of 81,723 (1.384 M of 1.40 M units). Flat strings above 4,096 units are almost all wide (they hold U+2019 and similar): only 0.5 M of the 7.21 M flat units are narrow. The idea "the docs are ASCII" is wrong for these pages.

### 5.3 DOM arena (main, end of journey)

195,966 nodes of 36 B (6.73 of 13.01 MiB capacity); text buffer 3.34 of 5.61 MiB; 102,149 attributes of 12 B. All of it belongs to `DOMParser` documents. `DOMParser.parseFromString` builds a temporary document, then `import_node` copies every node into the one arena of the page. The arena never frees a node, and the text buffer never frees text. A collect does not free these documents.

## 6. Designs

### 6.0 What the four engines do with the same data

I read the pages and headers named in the table. Cells without a source are from general knowledge of the engines. I did not re-measure any engine. Sizes marked "about" are my arithmetic from struct fields.

| Data | V8 | SpiderMonkey | JavaScriptCore | QuickJS | bare now |
| --- | --- | --- | --- | --- | --- |
| Value | 8 B tagged; 4 B with pointer compression. Compression cuts the heap by up to 43% (<https://v8.dev/blog/pointer-compression>) | | 8 B (JSValue) | 16 B `JSValue` | 16 B `Value` enum |
| Array of one kind | Elements kinds: `PACKED_SMI_ELEMENTS` is a `FixedArray` of small integers, `FixedDoubleArray` for doubles, plain `FixedArray` for the rest (<https://v8.dev/blog/elements-kinds>) | | | Fast array: `JSValue*` buffer, 16 B each, `count` and `size` in the object | 16 B each in a `Vec` for every kind |
| String | One-byte or two-byte character storage; a rope is a separate string kind | `JSString` has a header with length and flags. A Latin-1 flag. Inline strings keep up to 16 Latin-1 or 8 two-byte characters in the cell (thin) and 24 or 12 (fat). Ropes, dependent strings (a view into a base), external strings (`StringType.h`) | `JSString` is 16 B: cell header and one word (`StringImpl*`). `StringImpl` is 8-bit or 16-bit. A rope is 32 B with up to 3 fibers. A substring is a rope with a flag that points into a base (`JSString.h`) | `JSString` has a 12 B header, then 8-bit or 16-bit data. A rope is about 40 B (`quickjs.c`) | `Str` 24 B + 13 B slab + `Box<[u16]>` in malloc |
| Atom table | | One `JSAtom` per (length, characters): a linear string with the atom bit and a 32-bit hash. Pointer equality | | `JSAtomStruct` is a `JSString`. `atom_hash` is a `u32` table, `atom_array` is indexed by atom id | Interner: `u32` table at load 0.5, entries index `Slab<Str>` |
| Object header | | | | `JSObject` about 64 B plus an 8 B malloc header. The shape (with the prototype) is shared. Properties are a `JSProperty` array (16 B each) | 104 B `Object` in a `Slab` |

Observations that matter for the designs:

- All four engines store a string as 8-bit when it can. That halves the payload only if the text is Latin-1. The fetched pages here are not (U+2019). So 8-bit storage helps the atoms (1.3 MiB) and not the garbage.
- QuickJS keeps 16 B values and 16 B array elements. Its array record has no `Vec` header and no slab bookkeeping. V8 with pointer compression stores a small integer in 4 B.
- SpiderMonkey and JSC keep short strings in the cell or one block. bare allocates a second block for every atom.
- V8, SpiderMonkey and JSC use generational collectors, so short-lived garbage is cheap to free (general knowledge). bare cannot copy that now (section 6.4).

### 6.1 Design 1: share equal text, and fix the allocator threshold

Data structures.

- A weak table in `Heap`: `HashMap<(hash: u64, len: usize), StrId>`. A host call that makes a long string from bytes (`Body.text()`, `responseText`, `textContent`) asks the table first. On a hit, it compares the units with the bytes and returns the same `StrId`. On a miss, it makes the string and adds it. An entry is valid only while `strings.get(index, gn)` returns the string (a freed slot has a new `gn`). A collect drops the dead entries.
- One call in `limit_malloc_arenas` (`crates/net/src/parallel.rs`): `mallopt(M_MMAP_THRESHOLD, 256 * 1024)`. It is already the place of `M_ARENA_MAX`.

Algorithm and complexity. The hash is one pass over the bytes, 8 bytes per step. A hit costs one more pass for the compare and saves the UTF-16 copy and the allocation. Memory per entry: 32 B. Strings of 256 bytes or fewer skip the table.

Why this removes the growth. #497 made equal long strings separate. The table makes the host-made ones equal again, at a lower cost than interning: no table probe for every string, no hash for strings made by JS code. The threshold change removes the allocator part (section 4.3).

Measured (experiment build on main `28a8fbc2`, 5 runs each, peak RSS in MiB):

| Setting | Peak RSS, median (MiB) | Against default | Instructions |
| --- | --- | --- | --- |
| default | 97.5 | | 2,487 M |
| shared `text()` | 94.0 | -3.5 | 2,471 M (-0.6%) |
| `M_MMAP_THRESHOLD` 256 KiB | 91.5 | -6.0 | 2,484 M |
| both | 85.9 (range 85.8 to 86.7) | **-11.6** | 2,469 M (-0.7%) |

Allocator comparison on the same binary and journey: `libjemalloc.so.2` with `LD_PRELOAD` gives 121 to 123 MiB (2,468 M instructions). It is worse and I drop it.

The two together do more than the sum (-9.5). Reason (hypothesis): with every large block mapped on its own, a freed duplicate string goes back to the system. With the dynamic threshold, it stays in the brk heap.

Fixed threshold, other values (5 runs, this journey): 128 KiB 91.4; 256 KiB 91.5; 512 KiB 91.9; 1 MiB 93.0; 4 MiB 96.5; glibc default 98.5. Setting only `M_TRIM_THRESHOLD` or `M_TOP_PAD` (0) also gives 91 to 93 MiB, but the CPU time doubles (490 to 590 ms against 260 ms; hypothesis: a `brk` call for every growth). Do not use them.

Cost of the fixed threshold. Instructions do not change (47 journeys, threshold 128 KiB, 3 runs each: 95,377 M to 95,312 M, -0.07%). Page faults rise, because a freed large block is mapped again and zeroed. CPU time is higher. Two measurements, both on a loaded machine:

| Journey | CPU, `journeys.sh` cpu_ms, median of 5, two batches | CPU, `perf stat`, 6 runs | Page faults | Peak RSS, medians (MiB) |
| --- | --- | --- | --- | --- |
| `python-docs-search-results` | 262 and 258 to 265 and 264 (+1.5% to +2.3%) | +0.4% | 23.2 K to 28.1 K | 98.2 and 97.1 to 91.1 and 91.3 |
| `github-docs-home` | 611 and 611 to 639 and 631 (+3.3% to +4.6%) | +0.8% | 25.5 K to 52 K | 91.9 and 92.6 to 85.8 and 85.8 |
| `wikipedia-article` | 803 and 839 to 843 and 887 (+5.0% to +5.7%) | +5% | 14.4 K to 35 K | 59.5 and 59.3 to 53.6 and 53.3 |

The `journeys.sh` runs use a threshold of 128 KiB or 256 KiB (the file names do not say which: to verify). The two CPU measurements disagree for `github-docs-home` (+0.8% against +3.3% to +4.6%). The load changes between runs. Over the 47 journeys (128 KiB) the sum of median CPU is +2.8% (12,435 to 12,782 ms) and the sum of peak RSS is -3.9% (1,781 to 1,712 MiB). A fault costs about 2.2 microseconds. With a 1 MiB threshold `wikipedia-article` CPU is +1.6% and peak RSS 57.0 MiB, but this journey is then 93.0 MiB alone. Idle-machine CPU was not measured.

Risk. The threshold call is glibc only (Linux with glibc). On macOS, Windows and musl the allocator is another one: Design 1 gives only the -3.5 MiB there. The table needs one rule: no semantic change (strings are immutable; two equal strings give the same `===`).

Expected gain: criterion 6 from 98.2 MiB (main, bisect median) to about 86 MiB (-26% against 116 MiB). Instructions on this journey -0.7%.

### 6.2 Design 2: fewer bytes for each array and atom (portable)

This design makes no allocator call. It changes layouts.

Layout 2a: array elements in size-class blocks, a smaller record.

- An array record keeps the shape, the flags, the length and a `u32` handle to its elements. The elements live in slabs of fixed block sizes (1, 2, 3, 4, 6, 8, 16 values, then a `Vec`). The prototype moves into the shape (V8 and QuickJS do this). The dictionary pointer moves to a side table. The record is 40 B.
- Per array now: record 104 B + `Meta` 8 B + `back` 4 B + mark 1 B + one malloc chunk (8 B header, rounded to 16 B) for the elements.
- Per array in 2a: record 40 B + 13 B + elements in a slab block, no header.
- Arithmetic for 88,650 arrays: now 117 B x 88,650 = 9.9 MiB for records and about 16 B x 88,650 = 1.35 MiB of malloc headers and rounding. After: 53 B x 88,650 = 4.5 MiB. Gain: 9.9 + 1.35 - 4.5 = **6.7 MiB**. The elements (468,846 x 16 B = 7.15 MiB) stay the same.
- With 8 B values (a wide change; `design-js.md` section 6.2): the elements fall to 3.6 MiB, another -3.6 MiB. With a packed `i32` kind (as V8): the 239,779 elements of all-int arrays are 4 B: -2.7 MiB at 16 B values, -0.9 MiB at 8 B values. These two are optional and wide.
- Complexity: unchanged for reads. A write that grows the array moves it to the next size class (amortized O(1)).

Layout 2b: short atoms inline, Latin-1 payload.

- An atom keeps its characters in the `Str` when they fit in 16 B: 8 two-byte units or 16 Latin-1 units (the length and hash move into 8 B of the 24 B). No second block. 81,264 of 81,723 atoms are Latin-1. 44,252 atoms (54%) have 15 units or fewer (histogram bins 0 to 7 and 8 to 15, before the collect).
- Arithmetic: the second block of a short atom is a 32 B malloc chunk (8 B header, rounded), so inline removes about 32 B x 44,252 = 1.35 MiB. Latin-1 halves the payload of the 81,264 narrow atoms: 1.384 M units x 2 B = 2.6 MiB become 1.3 MiB (-1.3 MiB). The two gains overlap a little (an inline atom has no payload left to halve). Total at most **-2.7 MiB**. With the flat strings, Latin-1 gives 1.8 MiB at the peak (counted from narrow units, not built).
- Flat strings: Latin-1 saves only 0.50 M units here (1.0 MiB at 2 B, 0.5 MiB at 1 B), because the large pages are two-byte. That is -0.5 MiB at the peak. Complexity: hash and compare work on bytes; `str_units` (`&[u16]`) has many callers and becomes a view type. `design-js.md` section 6.4 already says this is a wide change.

Layout 2c (needed with 2a and 2b): the shared text of Design 1, without the allocator call: **-3.5 MiB** (measured).

Arithmetic for Design 2 against 97.5 MiB (the default median): 97.5 - 3.5 (2c) - 6.7 (2a) - 2.7 (2b) = **84.6 MiB**. 2a and 2b count bytes of live data. Some of them may not show in RSS, because allocator layout moves RSS by 1 to 2 MiB (section 4.1). So I take 88 MiB as the planning value (hypothesis). Target 92.8, margin about 5 MiB. Without 2c: 88.1 by arithmetic. None of it is built or measured: the numbers are counts times bytes.

Effect on instructions: fewer allocations (up to 88,650 element blocks and 44,252 atom blocks fewer) and less copying. I expect a gain of 0.5% to 1% (hypothesis). The risk is the change of `Object`: `heap*.rs` uses the type in many places. The design review of P5 already accepted that cost.

### 6.3 Design 3: free what the page no longer uses

- The DOM arena gets a per-document free range. A `DOMParser` document is made by one native call, so its nodes, text and attributes are contiguous ranges. When no wrapper of the document is live at a collect, the nodes join a free list, and the ranges of text and attributes are released (`madvise` on whole pages, or a segmented buffer).
- The strings of the fetched pages die when their promise handlers end.
- Arithmetic: the resident parts of the DOM arena are 6.73 + 3.34 + 1.2 = 11.3 MiB, and the garbage strings are 14.0 MiB (about 10.5 MiB after the sharing of Design 1, which saves 3.5). If both were freed when the handlers end, the peak would fall by about 11.3 + 10.5 = 21.8 MiB, to about 76 MiB (arithmetic).
- This needs a collect (or a cheaper way to know what is dead) at every handler end. Today a collect costs 137 M instructions (section 6.4). The journey has about 20 fetch handlers. It cannot be done with the current collector: 20 x 137 M = 2.7 G instructions (+110%).
- It also needs `NodeId` to carry a document id, or a free-range list in the arena. Both touch the host code that holds a `NodeId` (I did not count the sites).
- Verdict: not now. It is the right shape for the long run (real browsers free a `DOMParser` document with its wrapper), but it needs a collector that does not walk the whole heap.

### 6.4 Collect on pressure: measured, not recommended

`Interp::gc_due` collects when the accounted bytes pass `max(8 MiB, 1.75 x live after the last collect)`. The rule only counts the accounted bytes, so garbage of up to 75% of the live heap is normal. I added a second trigger in an experiment: collect when the bytes of new flat strings since the last collect pass a limit (5 runs each, default allocator, no sharing):

| Limit | Peak RSS (median, MiB) | Instructions |
| --- | --- | --- |
| none | 97.5 | 2,487 M |
| 8 MiB | 94.5 (-3.0) | 2,633 M (+5.9%) |
| 4 MiB | 94.1 (-3.4) | 2,905 M (+16.8%) |
| 2 MiB | 92.0 (-5.5) | 3,176 M (+27.7%) |
| 1 MiB | 91.9 (-5.6) | 3,718 M (+49.5%) |

One collect costs about 137 M instructions (measured on the collect). The extra instructions in the rows match 1, 3, 5 and 9 extra collects (by division). The live heap has about 91,000 objects, 86,000 strings and 469,000 elements, so a collect costs about 200 instructions for each cell. The 1.75 factor is a budget that protects this cost. A design that makes a collect cheap (a young generation, or one mark pass) would remove the need for the factor. That is a design problem of its own and not part of this task. With the allocator threshold and sharing, a limit of 8 MiB gives 84.0 MiB (-1.9 MiB against 85.9) for 2,620 M instructions (+6.1% against 2,469 M): not worth it.

## 7. Comparison

| | Peak RSS (MiB) | Instructions | Code | Portable | Margin to 92.8 MiB |
| --- | --- | --- | --- | --- | --- |
| Default (main) | 97.5 to 98.3 | 2,487 M | | | -5 |
| 1: sharing + threshold | 85.9 (measured, 5 runs) | -0.7% | one table, one `mallopt` call | threshold only on glibc | 6.9 |
| 1a: sharing only | 94.0 (measured) | -0.6% | one table | yes | -1.2 (misses) |
| 1b: threshold only | 91.5 (measured) | -0.1% | one call | no | 1.3 |
| 2: layouts | about 88 (arithmetic) | hypothesis: -0.5% to -1% | `Object`, `Str`, interner | yes | about 5 |
| 3: free DOM and strings | about 76 (arithmetic) | +110% with today's collect | collector, DOM ids | yes | about 17 |
| Collect on pressure | 91.9 to 94.5 (measured) | +6% to +49% | one trigger | yes | rejected |

Design 1a alone misses the target. Design 1b alone meets it with a margin of 1.3 MiB, which is smaller than the spread of the allocator state (1 to 2 MiB between merges). Design 1 together has a margin of 6.9 MiB.

## 8. Recommendation

1. Build Design 1: the sharing table for host text, and `mallopt(M_MMAP_THRESHOLD, 256 KiB)`. Measure CPU of the 47 journeys on an idle machine before merge. If `wikipedia-article` (+5.0% to +5.7% CPU under load) is a cost the owner does not accept, use 1 MiB. This journey is then 93.0 MiB with the threshold alone. With the sharing added I did not measure it.
2. Keep Design 2 as the follow-up for margin and for platforms with another allocator. Build 2a first (it is the largest: 6.7 MiB).
3. Do not build a collect on pressure before the collector is cheap.
4. Add a check for peak RSS of this journey to the benchmark gate. The growth of #497 was found only at the final benchmark, because nothing measured this journey per merge.

Expected effect on the criteria (journey `python-docs-search-results`):

| Criterion | Now | After Design 1 |
| --- | --- | --- |
| 6: peak RSS (target 92.8 MiB) | 98.2 MiB (-15% against 116) | about 86 MiB (-26%): met |
| 2: instructions | 2,487 M | 2,469 M (-0.7%); other journeys not measured |

## 9. Limits and budgets that point at a design problem

- The GC factor 1.75 and the 8 MiB minimum protect a collect that costs about 210 instructions for each live cell. The fix is a cheaper collector, not another factor.
- `M_ARENA_MAX` 2 (`limit_malloc_arenas`) is a memory budget set for the network threads. The dynamic mmap threshold of glibc (up to 32 MiB on 64-bit; <https://man7.org/linux/man-pages/man3/mallopt.3.html>) is a budget that protects the speed of `malloc`. It costs 6 MiB here.
- The DOM arena has no free. A page that parses many documents with `DOMParser` or `createHTMLDocument` only grows.

## 10. Not measured

- CPU time on an idle machine. The page faults are counted (14 K to 52 K), but the loaded machine hides the cost.
- Windows, macOS and musl allocators. The threshold is glibc only.
- The sharing table on the other 46 journeys. I did not check how many have duplicate fetches.
- Whether `XMLHttpRequest.responseText`, `innerHTML` and `textContent` also make duplicates. The experiment shares only `Body.text()`.
- The residency of capacity (which pages of a `Vec` are touched). The arithmetic of Design 2 counts in-use bytes.
- The live site. Design 2 and 3 are not built.
- The cost matrix, test262 and `exec_diff`: nothing was built in the product.
- Why single merges show steps of 1 to 2 MiB of RSS with the same live bytes (hypothesis: allocator layout; 4 runs are too few to prove it).
- massif ran once, on main. The shares of section 5.1 are from one run (hypothesis: the allocation order does not change on the virtual clock). The massif build came from main before the final version of the patch.

## 11. Questions for the owner

1. Is a glibc-only `mallopt` in `limit_malloc_arenas` acceptable, given that the core must stay OS-independent? It sits next to the existing `M_ARENA_MAX` call and is `cfg`-gated.
2. Is +0.4% to +5.7% CPU (page faults, loaded machine) acceptable for -6 MiB on every journey, or do you want the 1 MiB threshold (less CPU, less gain)?
3. Do you want Design 2a (arrays in size classes) as a step of P5 now, or after criterion 6 is met by Design 1?
4. Do you want a design for a cheaper collector (a young generation) as its own task? It unlocks Design 3 and lets the GC factor go.

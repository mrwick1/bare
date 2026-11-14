# Design: the peak itself, when the collector runs, and the `DOMParser` documents (criterion 6)

Task R2-C2 of perf goal round 2. This document changes no product code. The owner reviews it before anything is built.

Owner decision (2026-11-14): do not use the glibc mmap threshold (`mallopt(M_MMAP_THRESHOLD)`). Design the peak itself. This document follows that decision. I read the decision as also excluding `malloc_trim`, `M_TRIM_THRESHOLD`, `M_TOP_PAD` and every other allocator setting (question 5). Section 7 lists the allocator facts that I measured, only to explain the peak.

## 1. Summary

Criterion 6 asks for a peak RSS of `python-docs-search-results` that is 20% under the baseline. On the settled corpus (`virtual-settled`) the baseline is 113.4 MiB, so the target is **90.7 MiB or lower**. Main (`b9c89248`, after PR #533) gives **94.5 MiB** (5 runs: 93.6 to 94.9).

- **The collector is not the lever.** It runs once in the journey. It does not run again because it counts only JS heap bytes. The JS heap grows by 8.7 MiB after that collect. The process grows by 26 MiB. A forced collect at every 1 MiB of new JS data (9 extra collects, +52% instructions) lowers the peak by only 1.8 MiB (94.2 to 92.4). Garbage that is dead before the peak is 8.7 MiB accounted (8.0 MiB in malloc). Most of its memory stays in the process after it is freed (section 7).
- **The `DOMParser` documents are the lever.** The page parses 19 documents (5.15 MiB of HTML, 195,344 nodes). All of them are dead when their handler ends. At every collect in the journey, 1 tree of 483 to 1,623 has a live wrapper (the page itself, 0.4% of the nodes). The DOM arena never frees a node, so it keeps all 19 documents: 11.6 MiB of nodes, attributes and text.
- **A prototype that frees dead trees (free list for node slots, rebuild of the text and attribute buffers) gives 82.4 MiB** (5 runs: 82.2 to 82.8) with a trigger that counts native bytes. That is **27.3% under the baseline** and 8.3 MiB under the target. A trigger without the reclaim gives nothing (93.6 MiB). The reclaim without a trigger gives nothing (one collect only).
- **CPU:** the prototype costs +197 M instructions (+8.0%) on this journey: 2,454 M to 2,651 M. No other journey calls `DOMParser`, so no other journey pays. A cheaper collect walk (section 4.3) takes 55 to 75 M instructions off each collect on three journeys, and nothing off the others.
- **Other items over 5% of the peak** (section 5): the cached copy and the parse copy of `searchindex.js` (3.75 and 3.69 MiB, one body twice, -3.7 MiB with one shared buffer), the object slab (14.5 MiB), atoms (7.4 MiB), array elements (7.1 MiB), free memory that malloc keeps (about 12 MiB), and file-backed pages (14.3 MiB, 9.7 of it code).

Recommendation (section 8): build R1 (the trigger counts native bytes and the DOM arena frees dead trees) and C1 (cheaper collect walk). Add S1 (one buffer for the cached body and the script source). Expected: **about 82 MiB with R1 + C1, about 79 MiB with S1**. The design needs no allocator call and works on every OS.

## 2. How this was measured

- Machine: archMachine, shared with other agents (load average 6 to 9). Instruction counts hold under load. RSS also held: the spread inside a row is 0.0 to 2.3 MiB (most rows under 1.5 MiB). Wall time is not used.
- Journey: `python-docs-search-results`, corpus `/data/bare-tmp/corpus/virtual-settled`, `BARE_CLOCK=virtual`, `BARE_SEED=1`. Every run had 0 replay misses and passed, except where a row says otherwise.
- Clean binary: `base-build origin/main --release -p bare-drive` at `b9c89248aa` (5 runs, `journeys.sh run`). Instruction spread 0.01%.
- Counting binary: the same commit with a temporary patch in `/data/bare-tmp/r2/c2/r2c2-counters.patch` (not in the repository). It reads `perf_event_open` (instructions of the page thread), `VmRSS`, `mallinfo2`, and logs one line at each fetch text, each `parseFromString`, and each collect (with its phases). The knobs are environment variables (`R2_SINCE`, `R2_EXT`, `R2_TRUNC`, `R2_FASTARR`, `R2_TEXTDROP`, `R2_NOKEEP`). With no variable set the patch gives the same RSS and instructions as the clean binary (3 runs: 94.2 MiB, 2,454.1 M; clean: 94.5 MiB, 2,454.0 M).
- Allocation sites: `valgrind --tool=massif` on the counting binary (one run; peak snapshot 78.1 MiB). `heaptrack` is not installed. I installed no package.
- Logs, TSV files and scripts: `/data/bare-tmp/r2/c2/out/` (`exp-*.tsv` for every row, `ev-*.log` for the timelines, `census47.tsv`, `fast47.tsv`).
- Sources I read for the engine comparison (downloaded 2026-11-14): `quickjs.c` (`js_trigger_gc`), V8 `src/heap/heap.cc` (`Heap::UpdateExternalMemory`) and `include/v8-internal.h` (`kExternalAllocationSoftLimit`), SpiderMonkey `js/src/gc/Scheduling.h` (`MallocHeapThreshold`) and `js/src/gc/GCRuntime.h` (`JSGC_MALLOC_THRESHOLD_BASE`), Blink `third_party/blink/renderer/core/dom/node.cc` (`SameSizeAsNode`).

## 3. The journey, with the memory timeline

The page loads `searchindex.js` (3.7 MB decoded). Then it fetches 19 result pages one after the other. For each page it reads `text()`, calls `new DOMParser().parseFromString`, removes `script`, `style` and `.headerlink`, reads `textContent`, and lower-cases it.

Timeline of one run (counting binary, `R2LOG=1`; `ev-1.log`). Instructions are of the page thread, counted from the first collect (the counter opens there, so the first row is 0).

| Moment | Instr (M) | RSS (MiB) | JS accounted (MiB) | DOM arena nodes |
| --- | ---: | ---: | ---: | ---: |
| First collect starts (after the data literal) | 0 | 69.0 | 34.45 used, 0 live before | 611 |
| First collect ends | 162 | 70.6 | 27.11 live | 611 |
| After parse 1 to 8 (small pages) | 193 to 290 | 70.7 to 70.8 | | 3,723 to 17,465 |
| After parse 9 (312 KB) | 351 | 73.1 | | 29,911 |
| After parse 14 (462 KB) | 763 | 85.5 | | 101,587 |
| After parse 19 (555 KB, last) | 1,305 | 96.5 | 35.86 used | 195,955 |
| Forced collect at the end (patch only) | 1,505 | 96.5 | 27.14 live (8.72 freed) | 195,966 |

RSS grows by 27.5 MiB after the first collect. The JS heap grows by 8.7 MiB in that time. The accounted bytes never reach the trigger (section 4.1).

At the end (`smaps_rollup` and `smaps`): RSS 98,820 kB (96.5 MiB). 82.2 MiB is anonymous and 14.3 MiB is file-backed (binary code 9.7, read-only data 2.9, libc 1.0, others). The page thread's malloc arena is one mapping of 45.2 MiB with 43.8 MiB resident. Five large blocks add 31.8 MiB (the object slab, the DOM arena vectors, the large strings). `mallinfo2` at the end: 34.8 MiB in use in arenas, 14.0 MiB free in arenas, 44.5 MiB in mapped blocks.

## 4. Question 1: when does the collector run?

### 4.1 The trigger now

`Interp::gc_due` (`crates/js/src/eval.rs`): `depth == 0 && stmt_frames == 0 && used_bytes > max(8 MiB, live_after_last_collect * 7/4)`. `used_bytes` is `Heap::used_bytes`: bytes of objects, strings and flat strings that the heap accounts. It does not count the DOM arena, the network cache, decoded images or any buffer outside the JS heap. The loader asks at each round (`browser_loader.rs`, `rounds`: `is_multiple_of(collect_every) || gc_due()`), and the window asks after each key or click (`browser_ui.rs`).

In this journey:

- `gc_live` starts at 0, so the first safe point after the data literal collects (used 34.45 MiB, live after 27.11 MiB, 7.35 MiB freed, 162 M instructions).
- The next limit is 27.11 x 1.75 = 47.4 MiB. The JS heap reaches 35.86 MiB at most. No second collect runs.
- Garbage at the end: 8.72 MiB accounted (24% of the accounted bytes). `mallinfo2` in use falls from 34.8 to 26.8 MiB after the forced collect (8.0 MiB).

The 1.75 factor lets garbage grow to 75% of the live size. Here that is 20 MiB. This is a budget that protects the cost of a collect (section 4.3), as `design-js-memory.md` section 9 says.

### 4.2 What the other engines count

I read these in the sources named in section 2.

| Engine | What starts a collect | Counts native memory? |
| --- | --- | --- |
| QuickJS | `js_trigger_gc`: collect when `malloc_size + size > malloc_gc_threshold`. The threshold starts at 256 KiB. After a collect it is `malloc_size * 1.5`. | Yes. `malloc_size` is all memory that the runtime allocates, also buffers. |
| V8 | Allocation limit of the old generation, with a young generation for short garbage. `Heap::UpdateExternalMemory(delta)` adds native memory (for example array buffers and DOM). It starts incremental marking when the external total passes a limit (`kExternalAllocationSoftLimit` is 64 MiB over the low point since the last collect), and a collect when the heap limit is also reached. | Yes, by an explicit call from the embedder. |
| SpiderMonkey | A zone collect starts when the GC heap or the malloc heap of the zone passes a threshold. `MallocHeapThreshold` is a constant multiple of the retained malloc bytes after the last collect, with a base set by `JSGC_MALLOC_THRESHOLD_BASE`. | Yes, a second counter next to the GC heap. |
| bare now | `used_bytes > max(8 MiB, 1.75 x live)` | **No.** |

All three count native memory. bare counts the JS heap only, so a page that keeps its memory outside the JS heap (a DOM, a decoded body) never reaches the limit.

### 4.3 What one collect costs

I split one collect with `perf_event_open` around each phase (first collect, 34.45 MiB used):

| Phase | Instructions (M) | Share |
| --- | ---: | ---: |
| `Interp::collect` walk and the prunes of the side tables | 116.8 | 72% |
| `Heap::collect` (mark and sweep) | 44.9 | 28% |
| Trim and roots | 0.6 | 0.4% |
| Total | 162.3 | |

Later collects: walk 160 M (includes a census counter of mine), heap 26 to 29 M. One extra collect costs 140 to 151 M in the runs below (the `since` rows). The walk is the larger part. For every object it pops, it probes about 12 side tables (`funcs`, promises, host `trace`, views, proxies, `sc`, `intl`, weak tables, classes, async, generators). This is about 1,500 instructions for each of the 91,000 objects. 88,650 of them are data-literal arrays. An array cannot be a function, a promise, a view, a proxy or a host object. The walk also copies every element of every array into a work list, where `Heap::collect` visits it a second time. `perf record` of a run with 9 collects shows `Heap::stored_edges` (9.0%) and `Vec::extend_desugared` (3.5%) on top, then `Heap::collect` (5.6%) and `Interp::collect` (4.5%).

### 4.4 Designs for the trigger

**T1: count native bytes (recommended).** `Heap` gets a counter `external_bytes`. The host adds to it when it keeps native memory that only the collector can release: the nodes, text and attributes of a parsed or detached tree (`parseFromString` adds `nodes x 36 + text bytes + attribute slots x 12`), and later decoded bodies. The trigger becomes: `used_bytes > max(8 MiB, 1.75 x live)` or `external_since_last_collect > max(4 MiB, 25% of external_live)`. A collect resets the counter and the host lowers it by what a reclaim frees. Data: one `usize`. Cost: one add per parse. It is the rule of QuickJS (all malloc), V8 (`UpdateExternalMemory`) and SpiderMonkey (second counter), with a limit in bytes.

Measured (reclaim of section 6.2 in the prototype; a fixed limit, the 25% rule is not measured; 3 runs unless noted):

| Native-bytes limit | RSS median (MiB) | Range | Instr (M) | Change |
| --- | ---: | --- | ---: | ---: |
| 1 MiB | 76.7 | 76.1 to 76.9 | 3,843.5 | +56.6% |
| 2 MiB | 79.7 | 79.5 to 79.9 | 3,189.5 | +30.0% |
| 4 MiB | 82.0 | 82.0 to 82.1 | 2,869.8 | +16.9% |
| 8 MiB | 89.9 | 89.7 to 90.0 | 2,696.3 | +9.9% |
| 4 MiB, with C1 (section 4.5), 5 runs | **82.4** | 82.2 to 82.8 | **2,651.5** | **+8.0%** |
| 6 MiB, with C1 | 87.4 | 87.3 to 87.8 | 2,554.2 | +4.1% |
| 2 MiB, with C1 | 80.0 | 79.4 to 81.2 | 2,831.6 | +15.4% |

Base: 2,454.0 M and 94.5 MiB. At 4 MiB the journey runs 2 extra collects (reclaim 15 to 23 M each in the prototype). At 8 MiB it runs 1 or 2. The trigger alone, with no reclaim, gives 92.9 MiB (2 MiB limit, 3,133.6 M) and 93.6 MiB (4 MiB limit with C1, 2,599.6 M).

The limit is a budget: 4 MiB of parsed documents is about 2 documents of this journey. A lower limit saves 2 to 5 MiB more for 8% to 49% more instructions. I recommend 4 MiB with C1, and a rule that follows the live size (25% of live native bytes, at least 4 MiB), so a page with a large live DOM does not collect again and again.

**T2: count JS bytes since the last collect (a nursery-size rule).** `used_bytes - live_after_last > N`. This is what V8 and SpiderMonkey do for the young generation, but they pay a cheap minor collect. bare pays a full collect (140 to 151 M). Measured on the same journey (3 runs, no reclaim):

| `N` | RSS median (MiB) | Range | Instr (M) | Change | Extra collects |
| --- | ---: | --- | ---: | ---: | ---: |
| 8 MiB | 94.6 | 94.1 to 95.7 | 2,605.4 | +6.2% | 1 |
| 4 MiB | 93.3 | 93.2 to 93.3 | 2,744.7 | +11.8% | 2 |
| 2 MiB | 92.6 | 92.1 to 92.8 | 3,018.8 | +23.0% | 4 |
| 1 MiB | 92.4 | 91.1 to 93.4 | 3,720.2 | +51.6% | 9 |

The best row lowers the peak by 1.8 MiB (hypothesis: the freed blocks are big flat strings, malloc keeps them and the next blocks have other sizes; section 7). It costs 1.27 G instructions. I reject T2. It is a rule that protects the cost of a collect (a budget that points at a design problem, section 9).

**T3: a cheaper collect, so that a short interval costs little.** See C1 and C2 in section 4.5. This does not replace T1: T3 alone cannot free the DOM arena.

### 4.5 Designs for a cheaper collect

**C1: the walk skips the side-table probes for plain arrays (recommended, small).** `Interp::collect` tests `is_array && exotic == 0` before the 12 probes and goes straight to `stored_edges` and the prototype. Weak tables are the one risk: an array can be a `WeakMap` key. Keep one probe, guarded by a flag that the first `WeakMap`, `WeakSet`, `WeakRef` or `FinalizationRegistry` with an array key sets. Measured with a prototype that skips the weak probe, so a page with arrays as weak keys needs the flag:

| Measure | Before | After |
| --- | ---: | ---: |
| Walk of the first collect (M instr) | 116.8 | 63.2 |
| Instructions, 2 extra collects (`since` 4 MiB, 3 runs) | 2,744.7 M | 2,593.9 M (-150.8 M, 75 M for each collect) |
| 47 journeys, 1 run each, sum of instructions | 47,685.9 M | 47,699.4 M (+0.03%) |

Only three journeys changed: `python-docs-search` -4.46% (-56 M), `python-docs-search-results` -2.29% (-56 M), `go-dev-home` -0.38% (-4 M). The others have no change: their collects hold few arrays. The +0.03% in the sum is the cost of my prototype (one environment lookup for every object the walk pops, `env_u` is 1.8% of the profile); the real flag costs nothing. Gain: 55 to 75 M for each collect of a journey with data literals, 0 elsewhere.

**C2: one traversal instead of two.** `Interp::collect` walks the graph to find roots in the side tables and to mark environments. `Heap::collect` then marks the same graph again. A single work list could mark objects and fire the side-table hooks only for objects with a "has side state" bit. The bit is set where a side table gets an entry for an object (`funcs`, promise, host wrapper, weak key). Expected cost of a collect: mark (26 to 29 M) plus the hooks for about 8,000 non-array objects (8,000 x 1,500 = 12 M) plus the prunes: about 45 M, against 140 to 151 M now (arithmetic, not built). C2 is wider than C1: every site that adds a side-table entry must set the bit, and a missed site frees a live object. I do not recommend it now.

**C3: a young generation with sticky mark bits and a write barrier.** After a full collect every survivor is old. A minor collect marks from the roots and from a remembered set of old objects that were written to, and sweeps young objects only. It makes a collect for each handler cheap (hypothesis: 10 to 20 M). It needs a barrier at every store of a reference into an object (property set, element set, the dense paths that write `elems` directly, environment bindings). The gain over R1 + C1 is at most 2 to 4 MiB of JS garbage (the T2 rows show how little JS garbage does for RSS). Not recommended now.

## 5. Question 3 first: the peak by item

Massif peak (78.1 MiB live) from the counting binary on main, and the share of RSS at the peak (96.5 MiB). The massif total does not include allocator headers, mapped capacity that nothing touched, or the free memory in arenas.

| Item | MiB | Share of massif peak | Note |
| --- | ---: | ---: | --- |
| JS object slab (`Heap::alloc_object`, 96,559 objects) | 14.50 | 18.6% | 104 B record + 13 B bookkeeping; capacity counted; 9.6 used |
| DOM node `Vec` (`import_node`) | 13.01 | 16.7% | 6.73 used, 195,966 nodes of 36 B; the rest is capacity |
| Atoms (`intern_slice`) | 7.44 | 9.5% | 81,723 atoms |
| DOM text buffer | 5.61 | 7.2% | 3.34 used |
| Flat strings (`new_flat`) | 7.68 | 9.8% | page texts; mostly garbage (8.0 MiB freed by a collect) |
| Array elements | 7.08 | 9.1% | 468,846 elements of 16 B |
| `Client::keep` (HTTP cache body) | 3.75 | 4.8% | the body of `searchindex.js` |
| `Rc<str>` copy of the script source | 3.69 | 4.7% | the same text, for lazy functions (`Program` source) |
| `PropMap::set` | 3.07 | 3.9% | |
| DOM attributes, UTF-8 copy in `string_arg`, `shrink_to_fit` copies | 1.4 + 1.06 + 1.0 | 4.4% | transient and parsed-document copies |

By lifetime (what each part is when the journey ends): the DOM arena (11.6 MiB used) is dead except 0.4%. The flat strings are dead except 0.15 M units. The cached body and the source copy are live. The object slab, atoms and arrays are the search index and are live.

Items over 5% of the peak and what to do about them:

1. **DOM arena (nodes, attributes, text: 11.6 MiB used, 22.1 MiB counted by massif with capacity and copies; 23%).** Question 2, section 6. Dead data.
2. **Free memory that malloc keeps: about 12 MiB (12% of RSS).** The thread arena has 43.8 MiB resident. About 32 MiB of it is in use (34.8 MiB in use in all non-mapped memory, less the main heap). After the forced collect the arenas hold 21.9 MiB free. A data layout does not remove this, but the reclaim and fewer large short-lived blocks lower it (free memory at the end: 14.0 MiB on main, 10.9 MiB in the prototype run, with RSS 14 MiB lower). Section 7.
3. **Object slab, atoms, array elements (29 MiB, 37%).** The search index data. `design-js-memory.md` section 6.2 has the layouts (2a: size-class element blocks and a smaller record, -6.7 MiB; 2b: inline Latin-1 atoms, -2.7 MiB; arithmetic, not built). They are the portable follow-up if the owner wants more margin than R1 gives.
4. **The same body twice: 3.75 + 3.69 MiB (9.5% together).** The HTTP cache (`Client::keep` copies the body with `to_vec`) and the lazy-parse source (`Rc<str>` made from the `String` of the script) hold the same 3.7 MB text. Design S1 (section 6.5) keeps one copy. Expected -3.7 MiB (arithmetic; these are two mapped blocks of 3.7 MiB, so a freed block should return to the system: hypothesis, not measured).
5. **File-backed pages: 14.3 MiB of RSS (15% of the peak).** 9.7 MiB of it is the code of the binary: 89% of the code mapping is resident (hypothesis: fault-around maps 64 KiB for each fault). The rest is read-only data (2.9 MiB), libc and others. I dropped the code pages (the executable mapping only) with `madvise(MADV_DONTNEED)` at the first collect (prototype, 3 runs): peak 93.6 MiB (-0.6), +26 M instructions. At every collect of a 4 MiB `since` run: 91.0 against 93.3 MiB (-2.3), +71 M instructions. The pages come back as the code runs. I reject this: it is a hack on top of the allocator-like layer and it gains little. The real lever is a smaller binary. Not examined.

## 6. Question 2: the `DOMParser` documents

### 6.1 What the page does with them (measured)

- Count and size: 19 `parseFromString` calls, 12 different HTML texts. Equal texts: 95,023 B (2x), 312,518 B (2x), 348,516 B (2x), 462,428 B (2x), 510,258 B (2x), 555,419 B (3x). Sum 5.15 MiB of HTML. Documents of 1,023 to 19,949 nodes. Total 195,344 nodes (the arena at the end minus the 611 nodes of the page at the first collect), 102,143 attribute slots and 3.50 MB of text in the arena.
- Lifetime: a document lives for one fetch handler. At 7 collects (the first one, one forced at the end, and 5 triggered by the native-bytes limit in other runs) the census counted the trees that have a node with a live wrapper: always 1 tree, of 3, 483, 800, 1,149, 1,465 and 1,623 trees (the page itself: 608 to 844 nodes, 0.01 MiB of text). After the last collect 195,122 of 195,966 nodes (99.6%) belong to dead trees. No JS reference keeps a parsed document after its handler. The 1,600 trees are mostly the nodes that `el.remove()` detaches (script, style and `.headerlink` elements).
- Cost in bare: node 36 B (`parent`, `first_child`, `last_child`, `prev`, `next` as 4 B each, plus 16 B of data), attribute 12 B, text in one `String`. Over all parsed nodes: 36 + 6.3 (attributes) + 17.9 (text) = **60 B for each node**, 62 B with the rounding of the totals (11.6 MiB / 195,955 nodes).
- Cost in other engines: Blink asserts `sizeof(Node) == sizeof(SameSizeAsNode)`: a `ScriptWrappable` (vtable and wrapper reference), the event target data pointer, flags, two ints and four `Member` links. That is well over 36 B before `Element` data and attributes (hypothesis: 48 to 64 B for `Node` alone; the exact `sizeof` is not in the file I read). Gecko, Servo and Ladybird: I did not read their node layouts in this task. The size of a node is not the problem here: 60 B per node is lean. The problem is that the arena never gives a node back.
- Two copies at parse time: `parseFromString` makes a temporary `Document` (`bare_html::tree::parse`) and then `import_node` copies every node, attribute and text into the page arena. The temporary document and the UTF-8 copy of the argument (`string_arg`, 1.06 MiB) are freed at once, but they are 2 large short-lived blocks for each parse.
- The arena has caps (`max_nodes`, `max_text_bytes`, `max_attr_slots`) that protect a store that never frees. A page that parses a lot hits `dom-nodes` or `dom-text` although almost all of its nodes are dead. This is a limit that points at a design problem (section 9).

### 6.2 Designs

**R1: free dead trees in the page arena (recommended).**

Data structures.

- `Document` gets a free list of node slots (`Vec<u32>`). `new_node` pops from it before it pushes. A freed node is an empty comment that nothing links.
- Liveness comes from the collector. A tree is live when (a) it is the tree of a document the page still holds (the top node, shadow roots and template contents of live trees), or (b) any node of it has a live wrapper, or (c) native state names it (list below). The host computes this in `prune`, after `by_node.retain`. It climbs from each live wrapper to its tree root: O(wrappers x depth); the journey has 433 wrappers at the end.
- Dead trees are found from a list of tree roots that the document keeps (each `create_document` and each detach adds one; a root that gets a parent leaves the list). Freeing a tree walks its nodes once (about 20 instructions per node).
- Text and attributes: after a reclaim the host rebuilds `text` and `attrs` from the live nodes, in node order, and fixes the spans (a `Span` is `start` and `len`). It visits live nodes only, so it costs O(live nodes + live text bytes): in this journey about 800 nodes and 14 KB. It runs only when dead bytes are more than half of the buffer.
- Side maps keyed by node index (`foreign`, `class_sets`, `template_content`, `shadow_*`, `custom`, `states`, `ui`, images) drop the entries of freed indexes.

Algorithm. A collect runs (T1 or the old rule), `prune` finds live trees, `Document::reclaim(live_roots)` frees the others and compacts. Complexity: O(dead nodes + live nodes + wrappers x depth). Steady state: the arena holds the live nodes plus the nodes parsed since the last collect (about 4 MiB by T1), not all nodes ever parsed.

Native holders that must count as live (the prototype checks none of them): the incremental parser's open-element stack, `Range` and `Selection` boundary nodes, the focus and pointer targets (`user`), `inputs` and `ui` state, `MutationObserver` and `IntersectionObserver` targets and records, `NodeList` caches, style and layout tables indexed by node index, `sheet_made`, `sheet_scanned` and `sheet_list` (they scan by index, so a reused index must reset them), `mark_started` flags of scripts, the `watched` attributes, frames and `template_content`. A missed holder is a stale `NodeId` (a wrong node or a panic). The prototype found one such case: with a collect every 256 KiB of JS data it panicked at `dom.rs` (`index out of bounds: the len is 370 but the index is 862`) because a collect ran while a document was still loading. So R1 needs a test for each holder and a rule: no reclaim while a document parses.

Measured (prototype `r2_reclaim`, wrapper-only liveness, 3 runs unless noted): the table in section 4.4. Peak **82.4 MiB** (5 runs: 82.2 to 82.8), **-12.1 MiB** against 94.5. The arena stays at 84,979 nodes (3.0 MiB) from the first reclaim to the end, against 195,966 nodes. The mapped memory (`hblkhd`) stays at 25.8 MiB, against 44.5. A reclaim in the prototype costs 11 to 23 M instructions (it scans all nodes; the design visits only live nodes and dead trees, so I expect 1 to 3 M: hypothesis).

**R2: a separate `Document` for every parsed document, dropped with its wrapper.** `NodeId` carries an arena number in its high bits. `parseFromString` returns the parsed `Document` itself (no import copy, no temporary). The host holds a table of arenas by number. When the wrapper of the document is dead at a collect, the arena is dropped: its `Vec`s go back to malloc in 3 blocks. This is what Blink and Gecko do (the document owns its nodes and goes with its wrapper). Expected peak: the same steady state as R1, and it also removes the temporary document, the import copy and the 1.36 MiB of arena growth for each parse. It needs every `doc.borrow()` site of the `bare` crate to find the arena by `NodeId` (several hundred sites; I did not count them) and gives an extra indirection to every DOM access. Not recommended now. It is the shape for the long run. R1 does not block it.

**R3: a compact read-only tree for parsed documents.** A parsed document that no script has changed is kept in a frozen form: 4 `u32` for each node (parent, first child, next sibling, data) = 16 B, with text spans. It converts to the full form on the first write. Sphinx removes elements (a write), so the document converts at once on this page. Arithmetic for a page that only reads: nodes 36 to 16 B, so 7.07 to 3.1 MiB (-4.0 MiB at the end state). Without R1 this gives less than R1, and with R1 the nodes are only 1.4 MiB at a time. Not recommended.

**R4: parse into the page arena directly.** The parser (`Builder`) targets an existing `Document`, so there is no temporary and no import. It saves the two large short-lived blocks of each parse (the temporary document: up to 0.7 MiB of nodes and 0.36 MiB of text; the UTF-8 copy: 0.5 MiB). It needs a parser that does not own its `Document`: wide in the `html` crate. It does not lower the steady state. Not recommended.

**R5: truncate the arena after the last live tree.** I built this first as an experiment (`r2_truncate`). It cut 0 nodes at every collect: the page appends its own nodes (the result list) between the parsed documents, so the last live tree is always near the end. It gave 92.9 MiB (3 runs: 92.3 to 93.1) against 94.5. A suffix cut does not work. R1 needs the free list because live and dead trees interleave.

## 7. What the allocator does (measured, not a design)

The owner excluded allocator settings. These facts explain why some rows move less than the bytes they free.

- Forcing 9 extra collects frees 8.0 MiB of strings and lowers the peak by 1.8 MiB (section 4.4, T2). Hypothesis: the freed blocks are large flat strings (0.5 to 1.1 MB); malloc keeps them in the arena, and the next large block has another size.
- After the forced collect at the end, `mallinfo2` shows 21.9 MiB free in arenas (14.0 MiB before) and RSS does not fall.
- With R1 the arena frees 11 MiB of nodes and text, and RSS falls by 12 MiB. The arena `Vec`s are mapped blocks, so freed memory is not kept there. R1 also lowers the free memory in arenas from 14.0 to 10.9 MiB.
- The earlier measurement of `M_MMAP_THRESHOLD` (-8.0 MiB on top of sharing) is in `design-js-memory.md` section 6.1. It is not used.

## 8. Recommendation

1. **R1 + T1 (free dead trees; the trigger counts native bytes).** Peak 82.4 MiB measured in the prototype. Target 90.7 MiB. Margin 8.3 MiB. 27.3% under the baseline. It needs the list of native holders of section 6.2 and a failing test first: a page that parses 20 documents of 500 KB and drops each must keep the arena length under one document plus the live nodes.
2. **C1 (the walk skips plain arrays).** -55 to -75 M instructions for each collect on journeys with data literals. It pays for the 2 extra collects of T1: with it the journey costs +8.0% (2,651.5 M) and without it +16.9% (2,869.8 M).
3. **S1 (one buffer for the cached body and the script source).** -3.7 MiB by arithmetic. It is independent of R1.
4. Keep Design 2a and 2b of `design-js-memory.md` as the portable follow-up, and R2 as the long-run shape.
5. Do not build T2, C3, the code-page drop, or any allocator setting.

Expected effect on the criteria (journey `python-docs-search-results`, settled corpus):

| Criterion | Baseline | Main now | After R1 + T1 + C1 | After S1 as well |
| --- | ---: | ---: | ---: | ---: |
| 6: peak RSS (target 90.7 MiB) | 113.4 MiB | 94.5 MiB (-16.7%) | about 82.4 MiB (-27.3%): met | about 78.7 MiB (-30.6%, arithmetic) |
| 2: instructions of this journey | | 2,454 M | 2,651 M (+8.0%) | |
| 2: sum over the 47 journeys | | | +197 M (+0.4% of 51.6 G) by arithmetic: no other journey calls `DOMParser`. C1 takes 56 M off `python-docs-search` and 4 M off `go-dev-home` | |

The numbers are of a prototype with wrapper-only liveness. The build adds the checks for native holders. They may keep a few trees alive. I expect the peak to stay under 88 MiB (hypothesis).

## 9. Limits and budgets that point at a design problem

- The factor 1.75 and the 8 MiB minimum of `gc_due` protect a collect that costs 140 to 151 M instructions. C1 and C2 make the collect cheaper. T2 shows the budget at work: every row that lowers the peak by 1 MiB costs 2 to 3 collects.
- `max_nodes`, `max_text_bytes` and `max_attr_slots` cap an arena that never frees. With R1 they should count live nodes, not the length of the arena. I do not remove them in this task.
- `Heap::used_bytes` counts the JS heap only. It is a budget that cannot see the native memory that the heap causes.
- The 4 MiB limit of T1 is a budget too. It is the cost of a collect (75 to 100 M instructions) against the size of a document (1 to 2 MiB). A cheaper collect (C2, C3) would allow a lower limit.

## 10. Not measured or not verified

- The prototype frees by wrapper liveness only. The native holders of section 6.2 are not checked. A collect every 256 KiB panics (stale `NodeId`). The real peak with all holders is not known (hypothesis: under 88 MiB).
- CPU time on an idle machine. Only instruction counts (spread under 0.1%) and RSS are used.
- Other allocators: Windows, macOS and musl. All RSS numbers are glibc.
- Why timely collects free little RSS (section 7) is a hypothesis. I did not trace the free lists of malloc.
- S1 is arithmetic. `R2_NOKEEP` (no HTTP cache store) is not a valid experiment: the journey failed 3 of 3 (723 M instructions, 57.1 MiB), so the page needs the cache in replay (cause not examined).
- The C1 prototype skips the weak-table probe. A page with arrays as weak keys needs the flag.
- Gecko, Servo and Ladybird node sizes, and the exact `sizeof(Node)` of Blink: not read.
- The other 46 journeys were run once with the counting binary to count `DOMParser` calls (1 journey: this one) and collects (14 natural collects in all). I did not measure R1 or T1 on them. A page that parses many documents and keeps them live would pay the trigger without a gain.
- massif ran once, on the counting binary. The shares of section 5 come from one run.
- No product code changed: no cost matrix, test262, `exec_diff` or workspace tests were run.

## 11. Questions for the owner

1. Do you want R1 + T1 + C1 built as one PR or three? C1 is small and helps three journeys at once. R1 has the risk (native holders) and needs its own review.
2. Is a limit of 4 MiB of native bytes right? With C1, 6 MiB costs +4.1% and gives 87.4 MiB (3.3 MiB margin). 2 MiB gives 80.0 MiB for +15.4%.
3. Do you want S1 now, for -3.7 MiB? It changes the type of the cached body (`Vec<u8>` to a shared buffer) in `bare-net` and the source type in `bare-js`.
4. Is R2 (one `Document` for each parsed document) a step you want to plan, so that the import copy goes away too?
5. Does your decision on the mmap threshold also exclude `malloc_trim` and the other `mallopt` settings? I assumed yes and measured none of them.

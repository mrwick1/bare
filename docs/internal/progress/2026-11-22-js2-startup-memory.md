# 2026-11-22 — Reduce js2 startup memory

## Resident memory by mapping

Measure on archMachine with Linux 7.2.15-zen1-1-zen, Rust 1.96.0 and glibc 2.44. Use the [decision benchmark](2026-11-22-engine-benchmark-rerun.md) launcher sources and the raw `empty_1` fixture (`0`). Build js2 at `e3817f63`. Use the benchmark's stripped QuickJS direct binary (2026-06-04). Copy each binary with `cp --reflink=never` before each run.

A static tracer reads `/proc/<pid>/smaps` at the checksum write, before memory release syscalls, and at the exit stop. The table uses the median-total run of three independently for each stage. The larger snapshot shows blocks that shutdown later unmaps. The rows are in KiB.

| Mapping | js2, largest snapshot | QuickJS, largest snapshot | js2, exit | QuickJS, exit |
|---|---:|---:|---:|---:|
| Executable text | 2,148 | 780 | 2,148 | 780 |
| Executable read-only data and metadata | 576 | 200 | 576 | 200 |
| Executable data, bss and RELRO | 96 | 32 | 96 | 32 |
| Malloc brk heap | 40 | 156 | 40 | 156 |
| Anonymous mappings | 624 | 40 | 48 | 40 |
| Main stack | 40 | 16 | 40 | 16 |
| Shared libraries and loader | 2,396 | 2,244 | 2,516 | 2,244 |
| Kernel mappings | 12 | 12 | 12 | 12 |
| Total | 5,932 [5,908–6,196] | 3,480 [3,376–3,484] | 5,476 [5,412–5,748] | 3,480 [3,376–3,484] |

The text difference is 1,368 KiB at the selected peak snapshots. Read-only executable data and metadata account for 376 KiB. Anonymous mappings differ by 584 KiB. Of the js2 anonymous RSS, 576 KiB disappears during heap teardown in all three traces. This is nine 64 KiB blocks, each fully resident. The remaining anonymous mappings total 48 KiB.

## Malloc allocation sites

Massif runs separately on symbolized launchers for attribution, with `--peak-inaccuracy=0 --depth=20 --threshold=1`. These are live requested bytes at each engine's malloc peak. The native RSS trace accounts separately for js2 mappings from `bare_os::pages`.

| js2 allocation site | Bytes | QuickJS allocation site | Bytes |
|---|---:|---|---:|
| Shape node vector at built-in install | 6,144 | Small-allocation arena backing | 133,112 |
| Shape table vector on lazy-table install | 4,608 | Large allocations through `js_malloc_large` | 17,976 |
| Shape entry vector on property add | 1,632 | `js_def_realloc` | 3,096 |
| Lazy table slots in `Realm::lazy_table` | 1,536 | Runtime structure | 1,440 |
| Hash table growth | 1,308 | Other requests | 2 |
| Atom table slots | 1,152 | | |
| Other requests | 15,484 | | |
| Total live requests | 31,864 | Total live requests | 155,626 |
| Massif allocator overhead | 1,312 | Massif allocator overhead | 726 |

Malloc is smaller on js2. The largest identified groups are shape storage and lazy built-in registration. Prelude facts are loaded at the first prelude function lookup. The empty input does not call a prelude function.

## Designs

The preliminary native trace of the decision launcher showed 576 KiB of anonymous pages that disappeared during js2 heap teardown. The allocator maps each 64 KiB block on demand, then writes a free-list link in every cell when it first uses the block. Even a class with one live object faults in all 16 pages on Linux with 4 KiB pages.

| Part | Design | Expected RSS saving and CPU cost |
|---|---|---|
| Anonymous js2 blocks | Link only the first 4 KiB of each class's first block. Link the rest on demand or when the collector sweeps that block. Keep the existing address mask, mapping and collection accounting. | Hypothesis: about 500 KiB on an empty process. It skips most initial free-list writes. A class that uses the rest of its first block makes one extra slow-path call. The allocation fast path keeps one pointer load. |
| Anonymous js2 blocks | Give each class a shorter first mapping at a 64 KiB aligned address, then use full-sized mappings. Store the capacity in the header and keep the address mask. | Hypothesis: about 500 KiB resident and as much virtual memory. It needs one extra mapping and one more block for GC to track when a class grows. It needs different mapping support on Windows. |
| File-backed executable text | Order the startup functions together with a profile-guided linker order. | Hypothesis: save several 64 KiB fault-around windows, about 256–512 KiB if four to eight windows disappear. Instruction work is unchanged in principle. Layout can affect dispatch and cache behavior. This needs a reproducible profile and build tooling. |
| File-backed executable text | Outline cold paths from startup functions and dispatch, and keep them together. | Hypothesis: save 64–256 KiB if one to four fault-around windows disappear. Outlined calls cost extra instructions when reached. Measure parser, compiler and interpreter rows because outlining can also move hot instructions. |
| Read-only executable data and metadata | Pack relative ELF relocations with RELR. | The linker experiment saves 60 KiB of median read-only RSS. Startup instructions rise 1.68%. The release loader must support the format. |
| Read-only executable data and metadata | Pack the read-only constants that startup uses together. Move cold tables away from those constants. | Hypothesis: save 64–192 KiB if one to three neighboring fault-around windows disappear. Reading the same constants requires the same instructions. Extra indirection would add a load. |

Keep the 64 KiB blocks and defer free-list writes. This removes the block page writes that the trace found and preserves the collector's growth accounting. Treat executable layout as a separate change because it has different CPU and build risks.

A linker experiment establishes the size of the text-layout opportunity. Callgrind identifies 224 ELF text symbols executed by the js2 launcher. Their full bodies total 458,898 bytes. The executed instructions touch 154 distinct 4 KiB pages spread across 30 aligned 64 KiB regions. The largest function is the 171,804-byte interpreter dispatch. It executes 137 instructions in this trace. QuickJS touches 66 distinct 4 KiB pages across 11 aligned 64 KiB regions.

Relink the same Rust dependencies with LLD `--symbol-ordering-file`, using the symbols in the startup profile. Across three fresh copies, median text RSS changes from 2,184 [2,148–2,196] KiB to 632 [604–636] KiB. Median peak RSS changes from 5,756 [5,540–5,880] KiB to 4,236 [4,200–4,312] KiB. Minimum startup instructions change from 715,510 to 716,094 (+0.08%). This experiment uses a link-specific symbol list. In a second paired three-run sweep, the startup order raises the median of `loop_10` by 36 KiB and `calls_10` by 4 KiB. The spreads overlap, but these medians miss the scale-10 RSS gate. A production change needs a repeatable profile for browser builds and measurements on their hot paths. A separate RELR link shrinks the relocation sections from 95,616 to 3,584 bytes. Read-only RSS decreases from 572 to 512 KiB, and minimum startup instructions increase from 715,510 to 727,564. This exceeds the CPU gate.

## Heap change

A new block in a class with no swept blocks initially links the cells that fit in its first 4 KiB. Link at least one cell for the larger classes. The slow path links the rest when the prefix runs out. The collector skips the untouched tail when it visits dead cells and can link it when it sweeps. Charge the whole capacity at the first refill.

The initial prefix implementation increased the object micro by 0.588% on glibc and 0.585% on jemalloc. At 100,000 turns, Callgrind attributed 404,590 extra instructions to `Heap::alloc_slow`. The link loop loaded the size and multiplied it by the cell index on each iteration. The final loop loads the size once and walks the pointers at a fixed stride.

At the checksum stop, the nine block mappings occupy 156 KiB instead of 576 KiB. Anonymous mappings as a whole occupy 208 KiB instead of 624 KiB. Both figures were constant in three traces per build.

## Peak RSS

Run each case three times per build with alternating build order. Copy the unchanged decision launcher with `cp --reflink=never` before each run. Use the static `/data/claude-tmp/qjs-base/h/rss` wrapper and the raw scripts. Verify all nine completion values separately. The table gives median [min–max] in KiB. QuickJS direct on `empty_1` measures 3,244 [3,176–3,444] KiB with the same wrapper.

| Workload | Main | Heap change | Change |
|---|---:|---:|---:|
| `empty_1` | 5,776 [5,740–5,780] | 5,452 [5,372–5,828] | -5.609% |
| `loop_10` | 5,900 [5,776–6,096] | 5,464 [5,460–5,556] | -7.390% |
| `calls_10` | 5,908 [5,664–5,992] | 5,496 [5,364–5,604] | -6.974% |
| `objects_10` | 9,056 [8,924–9,200] | 8,436 [8,276–8,560] | -6.846% |
| `throws_10` | 7,636 [7,572–7,972] | 7,628 [7,548–7,864] | -0.105% |
| `closures_10` | 10,564 [10,464–10,596] | 10,408 [10,188–10,436] | -1.477% |
| `fib_10` | 5,856 [5,548–5,884] | 5,472 [5,392–5,572] | -6.557% |
| `webpack_10` | 7,856 [7,844–7,892] | 7,288 [7,272–7,316] | -7.230% |
| `concat_10` | 5,968 [5,676–6,180] | 5,500 [5,396–5,520] | -7.842% |

All eight scale-10 medians decrease.

## Instructions

Use `perf stat -e instructions:u` three times per build and case. The table gives minimum [min–max] in millions of instructions, and the change of the minimum. All rows pass the +0.5% gate in the default release build with both glibc and jemalloc.

| Workload and allocator | Main, M | Heap change, M | Change |
|---|---:|---:|---:|
| `empty_1 glibc` | 0.716536 [0.716536–0.716947] | 0.698008 [0.698008–0.698311] | -2.586% |
| `loopfn 3000000 glibc` | 800.278561 [800.278561–800.278747] | 800.257894 [800.257894–800.258291] | -0.003% |
| `calls 1000000 glibc` | 556.318175 [556.318175–556.318581] | 556.297668 [556.297668–556.298284] | -0.004% |
| `fib 27 glibc` | 241.962172 [241.962172–241.962914] | 241.940974 [241.940974–241.941947] | -0.009% |
| `closures 1000000 glibc` | 1,742.671827 [1,742.671827–1,742.672131] | 1,742.657268 [1,742.657268–1,742.658039] | -0.001% |
| `objects 1000000 glibc` | 699.661872 [699.661872–699.662341] | 699.641080 [699.641080–699.641987] | -0.003% |
| `empty_1 jemalloc` | 4.055395 [4.055395–4.056501] | 4.036783 [4.036783–4.037555] | -0.459% |
| `loopfn 3000000 jemalloc` | 803.673657 [803.673657–803.674113] | 803.653419 [803.653419–803.653844] | -0.003% |
| `calls 1000000 jemalloc` | 559.681803 [559.681803–559.682390] | 559.661150 [559.661150–559.662452] | -0.004% |
| `fib 27 jemalloc` | 245.353531 [245.353531–245.355422] | 245.333275 [245.333275–245.334464] | -0.008% |
| `closures 1000000 jemalloc` | 1,746.013730 [1,746.013730–1,746.014625] | 1,746.000004 [1,746.000004–1,746.000407] | -0.001% |
| `objects 1000000 jemalloc` | 703.024665 [703.024665–703.025203] | 703.003622 [703.003622–703.004091] | -0.003% |

## Correctness

The prefix allocation test fails on main and passes with the change. It checks growth accounting across the slow path. The dead-cell test checks visitation before and after a partial block's sweep. Linux passes 15 heap tests and 17 GC tests. Both new tests pass under Miri. The `bare-js2` typecheck passes.

Targeted test262 covers object and array expressions, WeakMap, WeakSet and ArrayBuffer at `e4fbf577`. Main and the change each have 1,251 passes, 6 failures and 412 skips. The lists of passing files match.

## Empty browser page

Release `bare-drive` opens an empty loopback HTML document in headless mode with `BARE_JS_EXEC=new` and `BARE_SEED=1`. An assertion verifies that the document body exists and has no children. All six assertions pass. Use a fresh copy and a static `wait4` wrapper for each sample. Median peak RSS goes from 20,972 [20,792–21,272] KiB to 20,796 [20,692–20,872] KiB. The spreads overlap.

The measured base is `e3817f63`. The final fast-forward to `f4ff9eed` adds only `docs/next-work.md`. Raw data is in `/data/bare-tmp/r22/startmem/` on archMachine. Not tested locally include Windows, the full workspace suite, the rest of test262 and windowed browser journeys.

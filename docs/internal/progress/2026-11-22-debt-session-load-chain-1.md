# 2026-11-22 Session load instruction debt

The old engine scanned all 32 target slots for each static host table row. Visit the set bits in target order. Skip bits for targets outside the supplied list. Keep the definition order and attributes.

## Cause

The one-codegen-unit build of `session_load chain 1` on `e97f1331` uses 70,354 more glibc instructions than `852c7036`. Callgrind shows an increase of 91,397 instructions. The dynamic loader's relocation routine at `0xfac0` accounts for 53,617. The binary has 32,138 relative ELF relocations, up from 27,960. The same routine increases by 51,505 instructions against the pre-#854 tree, `da9ba9af`.

The libc copy routine adds 22,310 instructions in Callgrind. Copies called by `Session::drive_loop` increase from 43,010 to 64,490. A session experiment removed four copies but saved only 2,133 retired instructions on glibc. It also changed the cookie site on resume. The final patch changes the host table loop only.

The host table loop accounts for 181,807 self instructions in Callgrind on `e97f1331`. The patch lowers this to 91,220. This recovers the startup cost while retaining both engines.

## Cost

Use archMachine, an AMD Ryzen 9 5900HX with Linux 7.2.15-zen1-1-zen and Rust 1.96.0. Run `session_load chain 1` three times in each release build and allocator. Count user-mode instructions with `perf stat -e instructions:u`. Use the minimum and set `glibc.malloc.tcache_count=65535` for both allocators. The BASE is `852c7036`. The before and after builds use main at `7de6ef4e`.

| Build | Allocator | BASE | Before | After | Before vs BASE | After vs BASE | Spread for BASE / before / after |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Release | glibc | 4,977,102 | 5,054,865 | 4,958,880 | +1.562% | -0.366% | 719 / 1,060 / 592 |
| Release | jemalloc | 8,945,936 | 9,030,188 | 8,934,028 | +0.942% | -0.133% | 1,185 / 910 / 1,851 |
| One codegen unit | glibc | 4,942,689 | 5,014,558 | 4,923,718 | +1.454% | -0.384% | 959 / 1,916 / 295 |
| One codegen unit | jemalloc | 8,915,129 | 8,989,280 | 8,899,370 | +0.832% | -0.177% | 1,805 / 1,492 / 491 |

Callgrind on the final default builds records 93,150 fewer self instructions in `install_table`. Run `cost_matrix.sh -j4 -a -J -g 'examples/(session_load|browser_load|adapter_load) '` against `7de6ef4e`. All 26 rows pass under both allocators and both build modes. The default matrix mean changes by -0.223% on glibc and -0.161% on jemalloc. The one-codegen-unit means are -0.217% and -0.157%.

The default build's peak RSS across three runs is 15,396–15,760 KiB before and 15,520–15,680 KiB after. The median moves from 15,560 to 15,564 KiB.

## Checks

The focused old-engine API tests pass 47 cases. The shared host table suite passes eight cases on both engines with and without forced collection. The browser session tests pass 33 cases. The js2 virtual session clock test passes. The `bare-js` and `bare` typechecks pass.

The `session_load` example hits its existing old-engine dereference when set to `BARE_JS_EXEC=new` on both sides. The virtual session test covers the js2 session path. Windows, Miri, test262 and the full workspace suite were outside this run.

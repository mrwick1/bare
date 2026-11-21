# 2026-11-21 Keep js2 in one release codegen unit

The full matrix found that `loopfn` rose 10.9% with the default release build and 0.35% with one codegen unit. On `1303414b`, changing only the codegen setting moved the count from 276 to 268 instructions per turn in `Vm::exec`. The seven bytecode ops per turn stayed the same. [Full matrix](2026-11-21-full-matrix.md)

Callgrind with `--dump-instr=yes` and `objdump` show that the default build executes 16 `movabs` instructions per turn, against 10 with one unit. Most load integer tags and masks. It executes 18 direct jumps against 15 and one fewer ordinary move. Both builds take seven indirect dispatch jumps per turn. The steady-state path executes zero alignment NOPs and zero out-of-line calls. The differences are in the machine instructions LLVM selects and places inside the loop. Each trace ran 20,000 turns. The individual LLVM pass responsible for each choice is unknown.

Both `Vm::exec` monomorphs were in object 00 of the default library archive. Each monomorph stays in one object. The `calls` profile shows an inlining difference. The default build calls `Realm::function` once per turn and spends 42 instructions in that function. With one codegen unit, it inlines the helper. It then spends 549 instructions in `Vm::exec` per turn, against 530 in the default build. Including the helper, the total falls by 23 instructions per turn. The default archive already has copies of this helper in objects 00 and 01. The difference comes from the inlining decision even with a copy in the interpreter object.

## Options

- Force inlining of `imul`, `imod` and `Realm::function`. This keeps the same codegen units and data layout. It cut `loopfn` by 1.37% but increased `objects` by 1.83% under both allocators. The integer helpers are already in the interpreter module and the measured `loopfn` path calls no helpers. The tag loads and direct jumps account for the gap. Alignment could affect cycles per instruction.
- Use a workspace-wide single unit or cross-crate LTO. The one-unit reference matches the existing cost fallback. Rust disables local ThinLTO when there is one unit under the default LTO setting. Cross-crate LTO changes the optimization and link scope for every release crate. Cargo requires a profile-wide LTO setting. [Rust codegen rules](https://doc.rust-lang.org/rustc/codegen-options/index.html#lto) · [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides)
- Give `bare-js2` one release codegen unit. It has the same hot loop count as the workspace-wide single-unit reference on this base. This option limits the override to the engine package. The hot `Vm::exec` symbol grows from 154,909 to 183,784 bytes.

The release workflow uses `cargo build --release --locked` for `bare-mcp` and `bare-drive`. CI runs tests and checks in their default profiles. The override goes in the workspace manifest under `profile.release.package.bare-js2`.

## Measurements

archMachine, Ryzen 9 5900HX, Linux, rustc 1.96.0. The patch base is `74f6611e`. The initial codegen comparison used `1303414b`. Use release builds with symbols kept. Each instruction count is the minimum of three runs of `perf stat -e instructions:u`. Use the tcache setting from `tools/js/instr.sh` and preload `/usr/lib/libjemalloc.so.2` for jemalloc. The interpreter figures subtract the minimum of three zero-turn runs. The spread column gives the largest sum of the full and zero-turn spreads across the columns. The workspace-wide setting is `RUSTFLAGS="-C codegen-units=1"`.

The arguments to `interp_load` are `loopfn 3000000`, `calls 1000000`, `fib 27`, `closures 1000000` and `objects 1000000`.

Startup runs `script_run` on `var x = 1;`. The bundle and eval rows use the files under `crates/js2/examples/data` with `script_run --eval` and results checked against Node.

### Measured base and candidate at `745d66e9`

| Allocator | Row | Base default | Package default | Max spread |
| --- | --- | ---: | ---: | ---: |
| glibc | `loopfn` | 874,500,400 | 850,500,818 | 1,828 |
| glibc | `calls` | 588,500,436 | 565,500,475 | 2,983 |
| glibc | `fib` | 259,571,831 | 249,402,101 | 1,072 |
| glibc | `closures` | 1,964,210,355 | 1,918,263,436 | 4,091 |
| glibc | `objects` | 711,260,694 | 712,262,248 | 2,042 |
| jemalloc | `loopfn` | 874,561,632 | 850,561,801 | 2,400 |
| jemalloc | `calls` | 588,529,831 | 565,561,359 | 33,335 |
| jemalloc | `fib` | 259,632,903 | 249,463,523 | 1,358 |
| jemalloc | `closures` | 1,964,180,181 | 1,918,233,103 | 28,932 |
| jemalloc | `objects` | 711,321,976 | 712,324,640 | 2,336 |

| Allocator | Row | Base default | Package default | Max spread |
| --- | --- | ---: | ---: | ---: |
| glibc | `startup` | 728,466 | 722,953 | 582 |
| glibc | `bundle_like` | 126,957,181 | 123,439,000 | 4,695 |
| jemalloc | `startup` | 4,068,544 | 4,062,699 | 1,043 |
| jemalloc | `bundle_like` | 130,624,212 | 127,052,672 | 14,831 |

Across the nine checked-in eval fixtures the changes ranged from -4.610% to -0.125% under the two allocators.

### Codegen comparison at `1303414b`

The package default column applies the override only to `bare-js2`. The CGU 1 columns use the workspace-wide flags, with and without the override.

| Allocator | Row | Base default | Force inline | Base CGU 1 | Package default | Package CGU 1 | Max spread |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| glibc | `loopfn` | 874,503,387 | 862,500,456 | 850,500,420 | 850,500,161 | 850,501,221 | 3,688 |
| glibc | `calls` | 588,500,379 | 568,500,765 | 565,501,097 | 565,499,923 | 565,500,561 | 1,301 |
| glibc | `fib` | 259,571,934 | 255,122,383 | 249,401,390 | 249,402,451 | 249,402,131 | 1,235 |
| glibc | `closures` | 1,964,216,802 | 1,913,216,707 | 1,918,268,887 | 1,918,269,382 | 1,918,268,642 | 4,490 |
| glibc | `objects` | 711,262,747 | 724,262,425 | 712,264,825 | 712,263,578 | 712,263,590 | 3,044 |
| jemalloc | `loopfn` | 874,561,087 | 862,562,133 | 850,561,046 | 850,561,072 | 850,561,986 | 3,122 |
| jemalloc | `calls` | 588,561,754 | 568,529,662 | 565,530,404 | 565,530,308 | 565,529,747 | 33,284 |
| jemalloc | `fib` | 259,632,969 | 255,184,310 | 249,462,853 | 249,463,619 | 249,461,725 | 32,043 |
| jemalloc | `closures` | 1,964,183,223 | 1,913,198,843 | 1,918,214,987 | 1,918,215,512 | 1,918,235,895 | 89,925 |
| jemalloc | `objects` | 711,322,412 | 724,323,192 | 712,324,380 | 712,323,287 | 712,324,448 | 2,442 |

From `1303414b` to `da9ba9af`, the package override micro counts moved by at most 0.006% under either allocator. Main gained the browser generic code and the atom probe change in that range.

### Historical matrix commits

The historical builds applied the same package setting through Cargo `--config`.

| Allocator | Row | 8875c60a, package CGU 1 | 852c7036, package CGU 1 | Max spread |
| --- | --- | ---: | ---: | ---: |
| glibc | `loopfn` | 847,501,087 | 850,500,407 | 1,167 |
| glibc | `calls` | 578,500,683 | 565,500,827 | 1,364 |
| glibc | `fib` | 250,673,593 | 248,765,790 | 1,772 |
| glibc | `closures` | 1,942,215,483 | 1,918,268,537 | 1,529 |
| glibc | `objects` | 719,291,593 | 712,262,128 | 981 |
| jemalloc | `loopfn` | 847,563,267 | 850,561,155 | 1,117 |
| jemalloc | `calls` | 578,530,238 | 565,529,857 | 33,753 |
| jemalloc | `fib` | 250,702,493 | 248,827,711 | 39,102 |
| jemalloc | `closures` | 1,942,164,672 | 1,918,273,245 | 64,019 |
| jemalloc | `objects` | 719,320,009 | 712,323,678 | 1,426 |

The `loopfn` change is +0.354% under both allocators with the package setting. The full matrix recorded +10.882% under glibc and +10.881% under jemalloc with the default settings.

### Browser rows and compile time

On `745d66e9`, `type_load` loaded the React fixture and checked the value after run with each of 0, 10 and 50 characters. Use `type_load /data/bare-tmp/triage/typing-page "#a1" CHARS`. Both builds used `BARE_JS_EXEC=new`.

| Allocator | Row | Base default | Package default | Max spread |
| --- | --- | ---: | ---: | ---: |
| glibc | `type_load_0` | 402,058,307 | 399,965,044 | 305,642 |
| glibc | `type_load_10` | 1,288,487,126 | 1,268,883,474 | 681,492 |
| glibc | `type_load_50` | 4,743,160,882 | 4,655,916,681 | 3,156,566 |
| jemalloc | `type_load_0` | 408,621,293 | 406,389,656 | 140,458 |
| jemalloc | `type_load_10` | 1,296,939,094 | 1,277,453,793 | 932,445 |
| jemalloc | `type_load_50` | 4,760,225,438 | 4,671,216,658 | 2,129,259 |

These old-engine rows were checked on `da9ba9af` before the js2 browser switch.

| Allocator | Row | Base default | Package default | Max spread |
| --- | --- | ---: | ---: | ---: |
| glibc | `dataset_load_keys` | 338,353,060 | 338,353,394 | 543 |
| glibc | `style_load_churn` | 303,060,620 | 303,062,112 | 38,210 |
| glibc | `web_apis_load_clone` | 1,353,245,608 | 1,353,243,717 | 8,066 |
| jemalloc | `dataset_load_keys` | 342,858,952 | 342,847,172 | 28,858 |
| jemalloc | `style_load_churn` | 310,216,787 | 310,037,421 | 77,457 |
| jemalloc | `web_apis_load_clone` | 1,418,495,778 | 1,418,401,287 | 779,979 |

The arguments are `dataset_load keys 1000 50`, `style_load churn 10` and `web_apis_load clone 10`. These rows run the old engine. The largest absolute difference was 0.058%.

| Cargo build | Default js2 | Package CGU 1 js2 | Default codegen phase | Package CGU 1 codegen phase | Default full command | Package CGU 1 full command |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `1303414b` | 49.9 s | 140.7 s | 19.5 s | 109.8 s | 178.9 s | 145.9 s |
| `da9ba9af` | 121.6 s | 238.5 s | 37.7 s | 161.0 s | 249.0 s | 247.1 s |
| `745d66e9` | 181.2 s | 186.9 s | 60.4 s | 121.8 s | 464.0 s | 248.4 s |

Cargo reported the times with four build jobs. The earlier builds used `cargo build --locked --release -p bare --example dataset_load --example style_load --example web_apis_load --timings`. The final pair used `cargo build --locked --release -p bare -p bare-js2 --example type_load --example interp_load --example script_run --timings`. Each cell is one observation on a shared machine. The full command times include different amounts of dependency reuse.

`cargo test --locked -p bare-js2 --lib` passed all 750 tests on `745d66e9` with one test thread. Earlier runs crossed the existing 20 s compile-time assertion at 24.096 s on `1303414b` and 21.574 s on `da9ba9af` during high host load.

The last merge from main brought in `74f6611e`. The unmodified base failed to build `bare-js2`: `bi_string.rs`, `bi_string_regexp.rs` and `regexp_rt.rs` still refer to the removed `F_REGEXP` flag. The instruction counts and passing tests use `745d66e9` as the newest buildable base measured here.

## Not tested

Windows runtime, Miri, test262, the library tests and performance after the last main merge, the full browser matrix, clean packaged release time, or IPC and branch misses on an idle host.

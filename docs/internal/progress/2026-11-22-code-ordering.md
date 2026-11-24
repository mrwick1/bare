# 2026-11-22 — Order code for startup

## Design

The [startup memory experiment](https://github.com/mrwick1/bare/pull/925) found 224 executed js2 text symbols on 154 pages. The temporary LLD order reduced median text RSS from 2,184 to 632 KiB. The linker can place whole function sections. A large dispatch function can still occupy many pages.

| Option           | Method                                                                                                                                                                                            | Cost and expected effect                                                                                                                                                                     |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| LLD symbol order | Put functions used by the empty workload first. Follow with functions shared by representative workloads, then group the functions specific to each workload. Use a separate list per executable. | Packs hot text on fewer pages. Adds no runtime work or Rust crates. Link-time inputs depend on the toolchain and code.                                                                       |
| Section grouping | Use stable `#[cold]` and `#[inline(never)]` on selected cold paths.                                                                                                                               | Can make startup-heavy functions smaller. Adds calls where code moves out of line. Needs edits across compiler, interpreter and browser.                                                     |
| PGO or BOLT      | Train a compiler profile or rewrite a linked executable with a sampled profile.                                                                                                                   | Can lay out individual blocks. Requires training and extra toolchain components. PGO adds instrumented and optimized builds. BOLT needs a compatible ELF binary and suitable profiling data. |

Use LLD symbol ordering. The release build wrapper applies it to x86-64 GNU/Linux only. Use the same compiler, inputs and linker for the unordered control. Keep other targets on the normal Cargo build path. No MSVC `/ORDER` setup.

The generator looks up each executed address in the sorted ELF symbol table with a binary search. It stores workload membership for each executed symbol. It sorts symbols once by their membership and address. The executable stores the same functions at new addresses.

Check three paired runs per input on fresh binary copies. Check the completion value or journey assertions before comparing peak RSS and user-mode instructions. Use the static wait4 wrapper for RSS and perf for instruction counts. Accept the ordered build when both empty processes fall clearly and each other RSS median stays within its measured spread. The instruction gate is +0.5%, with a one-codegen-unit comparison where necessary.

## Build and refresh

Run `tools/code-order/build.sh bare` for the browser CLI, `tools/code-order/build.sh bare-drive` for the journey runner, or `tools/code-order/build.sh script_run` for js2. The wrapper selects the stable x86-64 GNU/Linux release target. The release workflow uses it for the distributed Linux AMD64 driver. Plain Cargo builds, including CI debug checks, keep their existing link settings.

The order files record the Rust version from the profiled ELF. The wrapper warns when the build uses a different version. Refresh after a toolchain upgrade. As part of the periodic cost matrix, check symbol coverage and the empty RSS rows. Refresh when coverage drops below 90% or the RSS gain drops outside the original spread.

1. Build each executable with `tools/code-order/build.sh --plain --symbols BINARY`. Use the same Rust toolchain and Cargo feature selection as the release build. Keep the symbolized executable at a fixed absolute path. The wrapper keeps symbols only in the final executable. Setting `CARGO_PROFILE_RELEASE_STRIP=none` changes Cargo's mangled names across the build.
2. Set `BARE_SEED=1 BARE_JS_EXEC=new LC_ALL=C` for browser runs. Use an empty HTML document served over loopback for startup. Profile `bare --run URL` for startup and completed public page loads. Profile `bare-drive --quiet --var URL=URL tools/drive/public/NAME.steps` for the full journeys. Record public replies with `BARE_NET_FILL=DIR` and replay the same inputs with `BARE_NET_REPLAY=DIR BARE_CLOCK=virtual`. Unset the fill variable for replay.
3. For js2, first profile `script_run --eval empty.js 0` with `0` as the file contents. Generate the other programs with `cargo run --release --locked -p bare-js --example eval_load -- CASE 10 --source`. Pass the first output line as the expected result. Save the rest as the input file. Use `loop`, `calls`, `objects`, `throws`, `closures`, `fib`, `webpack`, and `concat`.
4. Prefix each executable command with `valgrind --tool=callgrind --dump-instr=yes --collect-jumps=no --compress-pos=no --compress-strings=no --callgrind-out-file=CASE.cg`. Use the absolute executable path, and keep only profiles that pass the workload checks.
5. Run `tools/code-order/order.py /absolute/path/to/BINARY empty.cg other.cg ... > BINARY.order.new`. The first profile supplies the startup group. The generator reads positive self instruction counts and uses `nm` to map addresses to linker symbols. On success, move the result to `tools/code-order/BINARY.order`. Link against it and repeat the paired measurements.
6. Run `tools/code-order/order.py /absolute/path/to/BINARY --check tools/code-order/BINARY.order` on a symbolized plain build to check coverage. LLD skips missing entries. The release wrapper refreshes the linker input path on every ordered build, so Cargo relinks the executable when the file changes.

## Symbol drift

In symbolized release builds with a common Cargo strip override and Rust 1.100.0, all startup symbols profiled before #925 survived the update from `f4ff9eed` to `e97f1331`. The counts were 1,061 in `bare`, 1,317 in `bare-drive`, and 258 in `script_run`. This compares one heap change and measures name survival.

At the same `f4ff9eed` commit, changing Rust from 1.96.0 to 1.100.0 retained 14 of 3,798 entries for `bare`, 14 of 4,698 for `bare-drive`, and 3 of 519 for `script_run`. Use the toolchain in the order header for a repeat build. Generate new orders when stable advances.

## Measurements

Build both variants from `e97f1331` on archMachine, AMD Ryzen 9 5900HX, Linux 7.2.15-zen1-1-zen, glibc 2.44, Rust 1.100.0 and its bundled LLD 23.1.5. Use locked dependencies, four Cargo jobs, and the same release profile and linker in both builds. Use the wrapper's default stripping for both measured variants. Keep symbols in a separate build for profiling. For every sample make a fresh `cp --reflink=never` copy. Alternate which variant runs first.

The standalone runner uses the [decision benchmark inputs and checksums](2026-11-22-engine-benchmark-rerun.md) through its `--eval` entry point. It checks the program's final expression by assigning it to a global. The browser runs headless with `BARE_JS_EXEC=new`. The empty case loads an empty document over loopback. The five journeys use their checked-in step files with recorded responses and the virtual clock. Train `bare` on the empty page and the DEV and Rust book entry pages. The full journeys use `bare-drive`. `bare --run` hit the js2 `clone_out` limit on React during the exploratory profile.

Each variant has three static wait4 RSS samples and three `perf stat -e instructions:u` samples per allocator. Use median peak RSS in KiB with the min–max range. Use the minimum retired user instructions with the min–max range. Use the repo's glibc tcache setting for instruction runs. The RSS runs use the normal glibc allocator.
Callgrind counted 707 executable pages with an executed instruction in empty `bare` before ordering and 290 after. The ordered profiling executable and the ordered release executable had identical `.text` bytes.

| Workload | RSS before, KiB | RSS ordered, KiB | glibc instructions, M | jemalloc instructions, M |
|---|---:|---:|---:|---:|
| `bare` empty page | 19,188 [19,104–19,328] | 8,744 [8,664–9,016] | 3.588 [3.588–3.588] → 3.586 [3.586–3.588] (-0.039%) | 7.644 [7.644–7.645] → 7.644 [7.644–7.645] (+0.003%) |
| `bare-drive` empty page | 20,832 [20,580–20,832] | 10,072 [9,956–10,232] | 4.094 [4.094–4.096] → 4.094 [4.094–4.095] (-0.001%) | 8.271 [8.271–8.272] → 8.269 [8.269–8.272] (-0.022%) |
| `react-docs` | 57,652 [57,176–58,804] | 47,692 [46,620–49,032] | 1,018.620 [1,018.620–1,040.092] → 996.732 [996.732–1,018.529] (-2.149%) | 1,007.141 [1,007.141–1,028.928] → 1,007.084 [1,007.084–1,008.291] (-0.006%) |
| `substack-home` | 63,620 [63,476–63,756] | 61,176 [60,700–61,716] | 1,613.759 [1,613.759–1,616.701] → 1,614.190 [1,614.190–1,614.959] (+0.027%) | 1,632.101 [1,632.101–1,633.883] → 1,633.570 [1,633.570–1,635.034] (+0.090%) |
| `devto-home` | 100,568 [96,488–106,140] | 90,024 [87,372–90,292] | 3,290.280 [3,290.280–3,290.876] → 3,290.290 [3,290.290–3,290.783] (0.000%) | 3,364.896 [3,364.896–3,366.014] → 3,365.210 [3,365.210–3,365.663] (+0.009%) |
| `rust-book-search` | 101,196 [101,080–101,576] | 90,796 [90,644–91,004] | 17,464.832 [17,464.832–17,493.988] → 17,467.179 [17,467.179–17,472.130] (+0.013%) | 17,573.592 [17,573.592–17,582.222] → 17,571.720 [17,571.720–17,575.411] (-0.011%) |
| `mdn-reference` | 71,176 [71,032–71,652] | 69,192 [68,768–70,852] | 5,561.939 [5,561.939–5,565.950] → 5,565.003 [5,565.003–5,570.703] (+0.055%) | 5,601.329 [5,601.329–5,602.806] → 5,605.755 [5,605.755–5,613.610] (+0.079%) |
| `empty_1` | 5,528 [5,340–5,632] | 4,300 [4,188–4,460] | 0.707 [0.707–0.708] → 0.707 [0.707–0.708] (+0.018%) | 4.044 [4.044–4.045] → 4.044 [4.044–4.045] (+0.005%) |
| `loop_10` | 5,484 [5,424–5,728] | 4,228 [4,152–4,320] | 740.814 [740.814–740.815] → 740.814 [740.814–740.815] (0.000%) | 744.186 [744.186–744.187] → 744.187 [744.187–744.187] (0.000%) |
| `calls_10` | 5,480 [5,356–5,480] | 4,416 [4,384–4,424] | 627.877 [627.877–627.878] → 627.878 [627.878–627.878] (0.000%) | 631.248 [631.248–631.248] → 631.247 [631.247–631.248] (0.000%) |
| `objects_10` | 8,536 [8,396–8,780] | 7,448 [7,204–7,508] | 550.119 [550.119–550.853] → 550.246 [550.246–550.439] (+0.023%) | 553.608 [553.608–553.975] → 553.944 [553.944–553.999] (+0.061%) |
| `throws_10` | 7,444 [7,200–7,768] | 6,552 [6,428–6,776] | 253.968 [253.968–253.969] → 253.968 [253.968–253.969] (0.000%) | 257.345 [257.345–257.345] → 257.345 [257.345–257.346] (0.000%) |
| `closures_10` | 10,064 [9,904–10,396] | 9,216 [9,028–9,340] | 113.177 [113.177–113.178] → 113.177 [113.177–113.178] (0.000%) | 116.551 [116.551–116.552] → 116.552 [116.552–116.553] (0.000%) |
| `fib_10` | 5,644 [5,492–5,648] | 4,432 [4,424–4,464] | 33.382 [33.382–33.383] → 33.382 [33.382–33.383] (0.000%) | 36.751 [36.751–36.752] → 36.750 [36.750–36.752] (-0.001%) |
| `webpack_10` | 7,212 [7,140–7,292] | 6,060 [5,988–6,208] | 44.342 [44.342–44.344] → 44.340 [44.340–44.343] (-0.004%) | 48.502 [48.502–48.503] → 48.502 [48.502–48.503] (+0.001%) |
| `concat_10` | 5,512 [5,176–5,536] | 4,276 [4,208–4,416] | 1.361 [1.361–1.363] → 1.362 [1.362–1.363] (+0.050%) | 4.704 [4.704–4.705] → 4.704 [4.704–4.704] (-0.006%) |

The empty `bare`, `bare-drive` and js2 runs fell by 54%, 52% and 22%. Every scale-10 program and public journey had a lower median RSS. The largest instruction increase was 0.090% on Substack with jemalloc. No row needed the one-codegen-unit recheck.

The five journeys passed their checked-in assertions. The instruction samples matched response URL, status and size except for up to four late avatar images on Substack with glibc. All Substack jemalloc samples had the same 142 responses. Train the driver on its empty page, React Docs, DEV and Rust book search. Hold out Substack and MDN.

## Validation

- The final plain release profiles resolve all order entries. The counts are 3,808 for `bare`, 4,713 for `bare-drive`, and 521 for `script_run`.
- Rust 1.100.0 passes the Linux debug check for `bare`, `bare-drive`, `bare-mcp`, and `bare-js2`. The equivalent Windows CI check passes through `cargo xwin check --locked --target x86_64-pc-windows-msvc -p bare-mcp -p bare-drive`.
- Bash syntax, Python bytecode compilation and release workflow YAML parsing pass. Sample logs are under `/data/bare-tmp/r22/order/` on archMachine.

Untested work includes native Windows builds and runs, Linux ARM release builds, the GitHub release job, live-network measurements, windowed browsing and the full test suite.

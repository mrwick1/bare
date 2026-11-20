# Design: time zone data on every OS

Status: proposed. It waits for the owner's review. No code is built. Written against main `43b62b05` on 2026-11-20. Machine: archMachine (AMD Ryzen 9 5900HX, 16 threads). Windows: the Windows 11 VM (build 26300, 26H2), local zone `Pacific Standard Time`.

## 1. Problem

On Windows the browser has no time zone data.

- Local time is UTC. `new Date(2020, 0, 1)` and `getHours()` use UTC.
- A named zone throws a `RangeError`. This hits `set_default_time_zone("Asia/Kolkata")`, `Intl.DateTimeFormat` with `timeZone`, and `toLocaleString` with `timeZone`.
- The cause: the code reads the IANA files from `$TZDIR` or `/usr/share/zoneinfo`. Windows has neither.
- Six tests fail on the VM, and PR #788 makes one of them skip. Five are in `bare-js` (`date_tests` 2, `intl_tests` 3; progress.md, the Windows VM notes). One is `browser_date_tests::local_time_uses_the_zone_of_the_realm_in_the_browser`. `tz_tests::system_files` already skips when it finds no files. The skips hide the bug. They do not fix it.

Logins and E2E journeys need the right local time: OTP windows, date pickers, "today", and the staging app, which calls `Intl.DateTimeFormat` with `Asia/Kolkata` at module level.

The owner rule (`decisions.md`, "Cross-platform core"): the core does not depend on the OS, and each engine step runs on the Windows VM and on Linux. The same file already lists "the local time zone and time zone data" as a `bare-os` service (cross-platform topic 2).

## 2. Current design

Code: `crates/js/src/tz.rs` (parser and rules) and `crates/js/src/eval_intl.rs` (loading and cache). The new engine (`crates/js2`) has no Date or Intl code yet. It will need the same data.

- **Where it is read.** `intl_zone` (`eval_intl.rs:668`). The UTC names and offsets (`+05:30`) need no file. Any other name must pass `is_file_name`, then the file `$TZDIR/<name>` or `/usr/share/zoneinfo/<name>` is read. `metadata` first (a regular file, at most 64 KiB, `MAX_FILE`), then `fs::read`.
- **How it is parsed.** `Zone::parse` reads TZif versions 1 to 4. It keeps the transitions (`Vec<i64>`, `Vec<u8>`), the offset of each time type (`Vec<i32>`) and the POSIX footer rule. It refuses leap seconds. Abbreviations and the DST flag are dropped. `America/New_York` (5 time types, 175 transitions) takes 1,595 B of heap.
- **How it is cached.** One `Rc<Zone>` for each name in the realm, at most 64 (`MAX_ZONES`, line 218). Each file read charges 10,000 steps (`ZONE_READ_STEPS`, line 298).
- **When.** Lazy. No zone is read until a local-time `Date` call or an `Intl` formatter needs one (test `local_time_zone_file_is_read_on_the_first_local_call_only`). A page that never asks for a zone pays nothing at startup.
- **How the local zone is found.** `intl_default_zone` (line 697): `set_default_time_zone` (tests), else `TZ` (a leading `:` is cut), else the target of the `/etc/localtime` link after `zoneinfo/`, else UTC. A regular-file `/etc/localtime` is not read (`resources.md`, "Local time"). The macOS link is `/var/db/timezone/zoneinfo/Asia/Kolkata`, so the same code works there (read on macbook-pro with a shell; bare was not run there).
- **Cost on Linux (measured, section 4).** 2.1 to 2.2 µs for `metadata` + `read` + `parse` of one warm file. 44 ns for one offset lookup.
- **On Windows.** No `TZ`, no link, no files. The result is UTC for local time and a throw for a name.

## 3. Measurements

Method. Throwaway programs outside the repository, built with `--release`. Linux: archMachine. Windows: the VM, programs cross built with `cargo xwin` and `-C target-feature=+crt-static`, run through `winssh`. Data: tzdata 2026f (`/usr/share/zoneinfo/tzdata.zi` of archMachine), compiled with `zic -b slim` and `zic -b fat`. Each timing is the best of 5 batches of 100,000 to 1,000,000 calls. I ran each program 3 times (Windows: 2 to 3 times). The machine was not checked for idle. Read the times as orders of magnitude.

### 3.1 Size of the data

| Data                                                               | Names | Distinct files |                                    Bytes |
| ------------------------------------------------------------------ | ----: | -------------: | ---------------------------------------: |
| `tzdata.zi` (zic source, text)                                     |       |                |                                 102.6 KB |
| zic `-b fat` (what Arch installs)                                  |   597 |            344 |      405,779 distinct (697,054 as files) |
| zic `-b slim`                                                      |   597 |            344 |      206,956 distinct (346,522 as files) |
| Own compact format (varint deltas, no abbreviations, no DST flags) |   597 |            344 | 96,256 (107,740 with its own name table) |
| Compact format, deflate (zlib 9)                                   |       |                |                                   23,975 |
| Slim TZif, deflate (zlib 9)                                        |       |                |                                   50,533 |
| Compact, transitions before 1970 dropped                           |       |                |                            70,360 (-27%) |
| Compact, only the 312 zones of `zone1970.tab`                      |   312 |            312 |                           94,034 (-2.3%) |

- 597 names are 344 distinct zones and 253 links. A link shares the file of its target.
- Fat files list every transition up to 2037. Slim files stop at the last rule change and `Zone::parse` uses the footer rule after it. Slim is enough for bare, and it halves the data.
- A subset by region saves 2 KB of 96 KB. The size is in the zones with many transitions (`Asia/Hebron` 310, `Asia/Gaza` 308, `America/St_Johns` 187), not in the number of zones. A subset also rejects names that Chrome accepts (`US/Pacific`).
- Dropping the years before 1970 saves 26 KB. It also removes facts that the tests use: `America/New_York` local mean time before 1883, and `Asia/Kolkata` war time in 1943. Dropping before 1900 saves 1 KB.
- For scale: `bare-mcp-windows-amd64.exe` of release v0.1.16 is 18,413,056 B. The linux-amd64 binary is 18,633,080 B.

### 3.2 Time

| Operation                                                            |          Linux |      Windows VM |
| -------------------------------------------------------------------- | -------------: | --------------: |
| `Zone::parse`, `America/New_York` slim (1,744 B)                     |  306 to 308 ns |         not run |
| `Zone::parse`, `America/New_York` fat (3.5 KB)                       |  381 to 385 ns |         not run |
| `Zone::parse`, `Asia/Kolkata` slim (220 B)                           |    67 to 68 ns |         not run |
| Find by name in an embedded table and decode, compact format         |  616 to 618 ns | 749 to 1,206 ns |
| Decode all 597 zones, compact format                                 |  302 to 344 µs |   517 to 607 µs |
| Offset lookup `Zone::offset`, now, `America/New_York`                |    43 to 44 ns |     46 to 58 ns |
| Local time to instant `Zone::to_utc`, now                            |         175 ns |          186 ns |
| Today: `metadata` + `read` + `parse` of one file (warm)              |  2.1 to 2.2 µs |  not applicable |
| `metadata` + `read` of one 1,744 B file (warm)                       |        1.67 µs |           57 µs |
| ICU `icu.dll`: `ucal_setMillis` + 2 `ucal_get` (one offset)          | not applicable |   352 to 358 ns |
| ICU: `ucal_open` + `ucal_close` for one zone                         | not applicable |   2.0 to 2.1 µs |
| Win32 `SystemTimeToTzSpecificLocalTimeEx` (local zone only)          | not applicable |   210 to 213 ns |
| Inflate the whole embedded blob (`miniz_oxide`, 24.0 KB to 107.7 KB) |         141 µs |         not run |
| Inflate slim TZif of all zones (50.5 KB to 207 KB)                   |  294 to 297 µs |         not run |

The raw slim parse (306 ns) plus a binary search over 597 names is about the cost of the compact decode, so a raw table lookup costs 0.4 to 0.6 µs on Linux (estimate, not measured as one piece).

### 3.3 Memory of the first lookup on Linux

A program with the compact blob in the binary read `RssFile` and `RssAnon` from `/proc/self/status` before and after one lookup. `RssFile` rose by 28 to 48 KiB in 3 runs. That is clean, file-backed, shared memory. It includes the kernel's read-around of 16 pages. The experiment also scanned the name table in order, so this is an upper bound. Before the first local-time call the data costs 0 pages. `RssAnon` rose by 32 KiB in the experiment, from the index vector of the experiment itself. The design does not build one.

### 3.4 What the OS gives on Windows (VM)

- `icu.dll` (2.8 MB) is in `System32`, with `icuin.dll` and `icuuc.dll`. ICU version 72.1. `C:\Windows\Globalization\ICU` holds `zoneinfo64.res`, `windowsZones.res` and `icudtl.dat`.
- `ucal_getDefaultTimeZone` and `ucal_getHostTimeZone` return the IANA name of the local zone: `America/Los_Angeles`. `ucal_getTimeZoneIDForWindowsID` maps `India Standard Time` to `Asia/Calcutta`, and `Nepal Standard Time` to `Asia/Katmandu`. Both calls need no table of ours.
- ICU gives correct offsets for old dates (`Asia/Kolkata` 1943: +6:30; `America/New_York` 1874: -4:56:02).
- `ucal_getTZDataVersion` returns `2022g`. The VM image carries tzdata from 2022. I compared ICU with the 2026f data for all 597 names, every 6 hours from 2000 to 2030. **31 names differ.** Examples: `Africa/Cairo` (Egypt DST of 2023), `Africa/Casablanca`, `America/Asuncion`, `America/Nuuk`, `Asia/Gaza`, `Asia/Almaty`, `Europe/Chisinau`, and, from 2026-11-01, 11 names: `America/Edmonton`, `America/Vancouver`, `America/Winnipeg`, `America/Havana` and seven more Canadian and Cuban names. The 566 other names agree, which also checks my pipeline. Hypothesis: a fully updated Windows has newer data. I did not test that.
- `ucal_open` with an unknown name does not fail. It returns UTC. A name check needs `ucal_getCanonicalTimeZoneID` (status `U_ILLEGAL_ARGUMENT` for `No/Such` and for `asia/kolkata`).
- Win32 `GetDynamicTimeZoneInformation` returns the key name of the local zone (`Pacific Standard Time`). The registry has 144 zones. It holds the rules of the Windows zone and its Dynamic DST years. It holds no history. `SystemTimeToTzSpecificLocalTimeEx` for the Pacific zone: 1883-11-18 gives -8:00 (tzdata: -7:52:58), 1974-02-01 gives -8:00 (tzdata: -7:00, the energy-crisis DST). Both are wrong. 1950, 1985 and 2000 are right. Hourly samples from 1990 to 2030 match tzdata with no difference. The .NET `TimeZoneInfo` (same registry data, hypothesis) gives `India Standard Time` +5:30 in 1943 (real +6:30), `Nepal Standard Time` +5:45 in 1980 (real +5:30), and `Eastern Standard Time` -5:00 on 1974-02-01 (real -4:00).
- CLDR `windowsZones.xml` (current `main`): 505 rows, 142 Windows ids with territory `001`. All 142 default targets exist in tzdata 2026f. The VM lists 144 ids. Two have no row: `Mid-Atlantic Standard Time` and `Kamchatka Standard Time`.

## 4. Designs

All designs keep `Zone::parse`, the offset code and the cache. The difference is where the bytes of a zone come from.

### A. Embed the data in the binary

A generated blob in the binary holds the slim TZif file of each distinct zone and a sorted table of the names. A lookup is a binary search, then `Zone::parse` on a slice. No file, no OS call, no `TZDIR`.

- Layout (generated): `u32 count`, `count` entries of 8 B sorted by name (`u16` name offset, `u32` data offset, `u16` data length), the names blob, the data blob. A link repeats the data offset of its target. About 221 KB with raw slim TZif: 206,956 + 4,776 + 9,093 + 4.
- Variant A2: the own compact format (varint deltas). About 108 KB. It needs about 25 lines of decoder. Decode is 530 ns against 308 ns.
- Variant A3: a subset (by region or by year). Section 3.1 says it saves 2 to 26 KB and loses names or history. Rejected.
- Variant A4: deflate the blob. 24 KB. It costs 141 µs and 108 KB of heap at the first use. It saves 84 KB of 18.4 MB. Rejected.
- Build: `tools/gen_tz.py` runs `zic -b slim` on `data/tz/tzdata.zi` and writes `data/tz/tzdata.bin`. The generated file is committed, as `tables.rs` is (`decisions.md`, "Unicode data"), so a build needs no network, no Python and no `zic`. A build script (`build.rs`) is not used. The repository has none.
- Update: run the generator when IANA publishes a release. A few times a year.
- Windows, macOS, Linux: the same bytes, the same results, on every machine.

### B. Use the OS on Windows

**B1. ICU in `icu.dll`.** `LoadLibraryW` and `GetProcAddress` at the first zone use (Windows 10 1903 and newer; hypothesis: Windows Server 2019 has no `icu.dll`). `ucal_open` per zone, `ucal_setMillis` and `ucal_get` per lookup. The local zone: `ucal_getHostTimeZone`.

- Binary: 0 B of data. Per lookup 352 ns against 44 to 58 ns. Per zone open 2.0 µs.
- `Zone` becomes a table or an FFI handle. The engine calls `offset` for each local-time step, so the hot path crosses an FFI. `to_utc` needs ICU's local-to-instant rules (`ucal_getTimeZoneOffsetFromLocal`, ICU 69 and newer). I did not test it. The ECMA-262 rules for a gap and an overlap must match `Zone::to_utc`.
- Data: the OS decides the version. The VM has 2022g and 31 of 597 names differ from 2026f. Two Windows machines can give two answers.
- Names: ICU returns old ids (`Asia/Calcutta`). A name check needs an extra call, and it must keep our rule (exact case).
- Linux and macOS keep their files. Two code paths. The core calls the OS for zone data, which the owner rule forbids for the core.

**B2. Win32 and the CLDR map.** `GetDynamicTimeZoneInformation` for the local zone. A table `IANA name to Windows id` (CLDR) for a named zone. Per lookup 210 ns.

- It covers the zones that the registry has, with no history before the Dynamic DST years. Section 3.4 shows wrong offsets for 1883 and 1974 in the Pacific zone, and the .NET results for India, Nepal and Eastern.
- A name that is not the local zone needs its own `DYNAMIC_TIME_ZONE_INFORMATION` from the registry. That is a registry reader and a rule evaluator. It is more code than A.
- Rejected: it fails the existing tests (`Asia/Kolkata` 1943, `America/New_York` before 1883) and it does not match V8.

### C. Ship the files next to the exe

Release assets are single binaries (v0.1.16: six executables). `offstage` installs those. C adds a `zoneinfo/` folder to each Windows release, found by `current_exe()`.

- Binary: 0 B. The folder: 346,522 B in 597 files (a zip has no hard links). Hypothesis: about 2.4 MB on disk at 4 KB clusters.
- Per zone load on Windows: 57 µs (measured `metadata` + `read`), against 1.7 µs on Linux and 0.4 to 0.6 µs for A.
- The failure mode of today returns: an exe that is copied alone has no data, and the zone silently becomes UTC. The offstage installer must change.

### D. Hybrid: system files first, embedded data as fallback

The Linux and macOS path stays as it is. Windows, and any missing file, use A.

- Two sources of truth. Linux uses the distro version, Windows uses the embedded one. A journey can give a different answer on two machines. The fallback is rarely used on Linux, so it is rarely tested there.
- It keeps the file read (2.1 µs, `is_file_name`, `MAX_FILE`, `TZDIR`) and adds A on top. It is the most code.
- A benefit: a distro updates its tzdata without a bare release. The owner's rule for fonts goes the other way: "no system font is read, ever", so the pixels do not depend on the machine. Time zones are the same kind of data.

### Comparison

|                              | A embed (raw slim)                                                                     | A2 embed (compact)       | B1 ICU                                    | B2 Win32 + CLDR               | C files beside exe                   | D hybrid             |
| ---------------------------- | -------------------------------------------------------------------------------------- | ------------------------ | ----------------------------------------- | ----------------------------- | ------------------------------------ | -------------------- |
| Binary size                  | +221 KB (1.2%)                                                                         | +108 KB (0.6%)           | +0 (code only)                            | +3.6 KB map                   | +0, files +346 KB                    | +221 KB              |
| Startup                      | 0                                                                                      | 0                        | `LoadLibrary` at first use (not measured) | 0                             | 0                                    | 0                    |
| One zone load                | 0.4 to 0.6 µs (Linux), about 0.8 to 1.3 µs (Windows)                                   | 0.62 µs / 0.75 to 1.2 µs | 2.0 µs                                    | none                          | 1.7 µs / 57 µs                       | as C, or as A        |
| One offset lookup            | 44 ns / 46 to 58 ns                                                                    | same                     | 352 ns                                    | 210 ns                        | 44 ns                                | 44 ns                |
| Memory                       | 1.6 KB heap per zone, at most 64 zones; up to 48 KiB clean file pages at the first use | same, fewer pages        | ICU heap (not measured)                   | none                          | same as A, plus a 1.7 KB read buffer | as A or C            |
| Correct for old dates        | yes (full history)                                                                     | yes                      | yes                                       | no (1883, 1974)               | yes                                  | yes                  |
| Update                       | regenerate and ship                                                                    | same                     | Windows Update (VM: 2022g, 31 names off)  | Windows Update, no history    | regenerate and ship                  | both                 |
| Windows                      | works on every build of Windows                                                        | same                     | 10 1903 and newer                         | works                         | needs the folder                     | works                |
| macOS                        | same bytes                                                                             | same                     | not used                                  | not used                      | folder                               | files, else A        |
| Linux                        | same bytes, lower cost                                                                 | same                     | not used                                  | not used                      | folder                               | files                |
| Same answer on every machine | yes                                                                                    | yes                      | no                                        | no                            | yes                                  | no                   |
| Code                         | one table, one lookup                                                                  | plus a decoder           | FFI, `Zone` becomes a trait               | registry reader and rule code | path search                          | A plus the file path |

## 5. The local zone on Windows

The local zone needs the OS in every design. The zone data does not.

1. `TZ`, when it is a name in the table. The same rule as on Linux. A journey can set it.
2. `GetDynamicTimeZoneInformation` in `kernel32`: the field `TimeZoneKeyName` (`Pacific Standard Time`). One hand-written `extern "system"` declaration in `bare-os`, as `trust.rs` does for `crypt32`. My experiment declared the `DYNAMIC_TIME_ZONE_INFORMATION` structure by hand and read the right key on the VM.
3. The key goes through a generated table of the CLDR `001` rows: 142 rows, about 3.6 KB (Windows id as a string, `u16` index of the IANA name). `Pacific Standard Time` gives `America/Los_Angeles`, the same answer as `ucal_getDefaultTimeZone`.
4. No row (2 of 144 ids on the VM): use a fixed offset from the `Bias` of the same call, or UTC. See open question 4.
5. Otherwise UTC, as today.

The territory rows (505 rows, 14.5 KB) would give `Europe/Amsterdam` for an Amsterdam user, where the `001` row gives `Europe/Berlin`. The offsets today are equal. The text of `resolvedOptions().timeZone` and the history before 1970 differ. Chrome gets the territory from ICU.

The generator reads `data/tz/windowsZones.xml` (CLDR; the same Unicode terms as `data/unicode/`; check the license text when the file is added) and fails when a target is not a name of the data. The 142 targets pass today.

The zone is read once, at the first use, and kept for the life of the realm, as on Linux. A zone change while bare runs is not seen.

## 6. Recommendation

Build **A with raw slim TZif**, on all three OSes, with the Windows local zone of section 5.

Why. It is the only design that gives the same answer on every machine, with full history, current data and no OS call for data (the owner rule for the core). The cost is 221 KB of read-only data (1.2% of the binary) and no start-up work. It is the smallest change to the code: the loader gets shorter, and it removes `is_file_name`, `MAX_FILE`, the `metadata` call, `TZDIR` and the file path. B1 is cheaper in size, but it is slower, stale on the VM and it changes the `Zone` type. C and D bring back the failure that this document is about.

Raw TZif over the compact format: no decoder, the unchanged `Zone::parse`, and a test that compares the embedded bytes with a system file. The lookup returns `&'static [u8]`. A move to the compact format later (108 KB) changes only that function.

### Build steps

1. Failing test first. Remove the skip that #788 added to `browser_date_tests::local_time_uses_the_zone_of_the_realm_in_the_browser`. Run it on the VM: it fails with the `RangeError` of today.
2. `data/tz/`: `tzdata.zi` (public domain), `windowsZones.xml`, a README with the zic version and the two licenses. `tools/gen_tz.py`: `zic -b slim`, dedupe by content, write `tzdata.bin` (section 4, A) and the Windows table.
3. New crate `bare-tzdata` (no dependencies): `include_bytes!`, `find(name) -> Option<&'static [u8]>` by binary search, `windows_zone(key) -> Option<&'static str>`, `VERSION`. Both engines can use it. `tz.rs` stays in `bare-js` until `bare-js2` needs `Zone`. Then it moves to a shared crate.
4. `intl_zone` in `eval_intl.rs`: replace the `metadata` and `fs::read` lines with `bare_tzdata::find`. Keep `ZONE_READ_STEPS`. Remove `is_file_name` and `MAX_FILE`.
5. `bare-os`: `tz::local_zone() -> Option<String>`. `TZ`, then the `/etc/localtime` link (code moves from `intl_default_zone`), then the Windows call. `eval_intl.rs` leaves the `known` list of `tools/portable_check.sh`. That is the goal of topic 2.
6. Tests that stay as they are run on every OS (they no longer need system files): `tz_tests::system_files` reads the embedded data.

### Checks (defined before the code)

1. `browser_date_tests::local_time_uses_the_zone_of_the_realm_in_the_browser` runs, and passes, on the VM and on Linux. It does not skip.
2. The five `bare-js` tests that fail on the VM today (`date_tests` 2, `intl_tests` 3) pass there. `cargo xwin test` of `bare-js` and `bare --lib`: 0 failed, 0 zone skips.
3. New tests: every name parses (`all_names_parse`); each target of the Windows table is a name (also a generator check); on Linux and macOS, when `/usr/share/zoneinfo/tzdata.zi` has the version of the embedded data, the offsets of all names match the system files for each 6 hours from 1970 to 2040 (`embedded_matches_system`); on the VM the local key maps to a name that has data.
4. VM, end to end. `bare-drive` journey with `new Date(2020, 0, 1).getTimezoneOffset()` and `Intl.DateTimeFormat().resolvedOptions().timeZone`: with no `TZ` it gives 480 and `America/Los_Angeles`; with `TZ=Asia/Kolkata` it gives -330. Then `tzutil /s "India Standard Time"`, run again with no `TZ` (-330, `Asia/Calcutta`), and restore the VM zone.
5. Linux cost: the cost matrix has 0 rows over +0.5%. `instr.sh` A/B on a journey with no local-time call: the same instruction count within noise (the blob is never touched). A journey with one local-time call: lower. RSS with `cp --reflink=never` copies. Release binary size on both OSes: +221 KB.
6. `tools/portable_check.sh`: the Windows and macOS builds pass, and the `known` list loses `js/src/eval_intl`.

## 7. Open questions for the owner

1. **Embed on every OS and stop reading system files (A), or keep system files first on Linux and macOS (D)?** Recommended: A. One source of truth, the same answer on every machine, less code. Linux then ignores a newer distro tzdata until the next bare release.
2. **Raw slim TZif (+221 KB) or the compact format (+108 KB, +25 lines)?** Recommended: raw. Switch only if binary size becomes a goal.
3. **Windows local zone: `001` rows only (3.6 KB), or all territories through the user's geo name (14.5 KB)?** Recommended: `001` rows only. The offsets are equal today. Add territories if a journey needs the exact IANA name.
4. **A Windows zone id with no CLDR row (2 of 144 on the VM): fixed offset from `Bias`, or UTC?** Recommended: the fixed offset. It is three lines and it is right for the current moment, where UTC is silently wrong.
5. **Code layout: new crate `bare-tzdata`, `bare-os::tz::local_zone`, `tz.rs` stays in `bare-js` for now?** Recommended: yes. It keeps the PR small and `bare-js2` can reuse the data crate.
6. **Who updates the data, and when?** Recommended: regenerate at a bare release when IANA has a newer version. One line in the release checklist, no CI job.
7. **Keep the 10,000-step charge for each zone load?** Recommended: keep it. The step counts and the virtual clock stay the same. No measurement asks for a change.
8. **Read a regular-file `/etc/localtime` on Linux (Docker images)?** Not part of this change. Recommended: a separate PR. The file is a TZif file, but it has no IANA name for `resolvedOptions().timeZone`.
9. **Accept zone names without regard to case (`asia/kolkata`)?** Today it is a known deviation from V8. With a table it costs one comparison. Recommended: not in this change.

## 8. Not tested

- macOS: the facts in section 2 come from a shell on macbook-pro. bare was not run there. The design does not read files there, so a macOS run is still needed.
- Windows 10 and Windows on ARM. Whether `icu.dll` is missing on Windows Server 2019. A fully updated Windows. ICU `ucal_getTimeZoneOffsetFromLocal`. The cost of `LoadLibrary` for `icu.dll`. ICU zone enumeration (`uenum_count` gave 0 in my probe; I did not look into it).
- B2 for a zone that is not the local zone. The .NET results stand in for Win32 on the zones that I could not select.
- Whole-browser numbers: all timings are from small programs. Instruction counts, RSS of the whole browser and the cost matrix come with the build.
- `zic` output across versions of `zic`: slim output has no known version dependence, but I used one version (the one on archMachine).
- Chrome or Node as an oracle for the 2026f data. The Node 22 corpus of `eval_date_tests` uses Node's own tzdata.

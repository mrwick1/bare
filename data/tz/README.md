# Time zone data

Sources of `crates/tzdata` (`python3 tools/gen_tz.py` builds `crates/tzdata/src/tzdata.bin` and `windows.txt` from them).

- `tzdata.zi`: the IANA time zone database, release 2026f, in the zic input format. Public domain. Source: https://www.iana.org/time-zones. This copy is the file `tzdata.zi` of the Arch Linux `tzdata` package 2026f. The generator runs `zic -b slim` (tzcode 2026f) on it.
- `windowsZones.xml`: the CLDR map from Windows time zone ids to IANA names, from https://github.com/unicode-org/cldr/blob/main/common/supplemental/windowsZones.xml (read 2026-11-20). Unicode License v3 ([../unicode/LICENSE.txt](../unicode/LICENSE.txt)). The generator keeps the 142 rows with territory `001`.

To update at a bare release: replace both files, set `VERSION` in `crates/tzdata/src/lib.rs`, run `python3 tools/gen_tz.py`, and commit the result. `python3 tools/gen_tz.py --check` fails when a generated file is out of date.

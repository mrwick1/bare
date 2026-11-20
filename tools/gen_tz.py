#!/usr/bin/env python3
"""Generate crates/tzdata/src/tzdata.bin and windows.txt from data/tz/.

Run from the repo root: python3 tools/gen_tz.py        (needs `zic` of tzcode 2026f or newer)
Check only:             python3 tools/gen_tz.py --check  (fails when a generated file is out of date)

Sources: data/tz/tzdata.zi (IANA tzdata, public domain) and data/tz/windowsZones.xml (CLDR). See data/tz/README.md.
To update: replace the two source files, set VERSION in crates/tzdata/src/lib.rs, run this script.

tzdata.bin (all numbers little-endian):
  u32 count, u32 names_len
  count x { u16 name_offset, u32 data_offset, u16 data_len }   sorted by name; a link repeats the data of its target
  names (no separators, in table order; a name ends where the next one starts, the last at names_len)
  data  (slim TZif files, each distinct file once)
windows.txt: one `Windows id<TAB>IANA name` line for each CLDR territory 001 row, sorted by id.
"""
import os
import re
import struct
import subprocess
import sys
import tempfile

SKIP = {"Factory"}  # a zic zone that is not a place
out_dir = "crates/tzdata/src"

zi = open("data/tz/tzdata.zi", encoding="utf-8").read()
version = re.match(r"# version (\S+)", zi).group(1)

names = {}
with tempfile.TemporaryDirectory() as tmp:
    subprocess.run(["zic", "-b", "slim", "-d", tmp, "data/tz/tzdata.zi"], check=True)
    for dirpath, _, files in os.walk(tmp):
        for f in files:
            path = os.path.join(dirpath, f)
            name = os.path.relpath(path, tmp)
            if name in SKIP:
                continue
            with open(path, "rb") as fh:
                names[name] = fh.read()
assert all(re.fullmatch(r"[A-Za-z0-9_+/-]+", n) for n in names), "unexpected file name"

order = sorted(names)  # bytewise order on ASCII, the order that `find` uses
table, data, offset_of = bytearray(), bytearray(), {}
name_blob = "".join(order).encode()
pos = 0
for n in order:
    blob = names[n]
    if blob not in offset_of:
        offset_of[blob] = len(data)
        data += blob
    table += struct.pack("<HIH", pos, offset_of[blob], len(blob))
    pos += len(n)
assert pos < 1 << 16 and len(data) < 1 << 32 and all(len(b) < 1 << 16 for b in offset_of)
bin_out = struct.pack("<II", len(order), len(name_blob)) + table + name_blob + data

rows = {}
for m in re.finditer(r'<mapZone other="([^"]+)" territory="001" type="([^"]+)"', open("data/tz/windowsZones.xml", encoding="utf-8").read()):
    other, target = m.groups()
    assert target in names, f"{other}: {target} is not a name of the data"
    rows[other] = target
assert len(rows) == 142, len(rows)
txt_out = "".join(f"{k}\t{rows[k]}\n" for k in sorted(rows)).encode()

for file, content in (("tzdata.bin", bin_out), ("windows.txt", txt_out)):
    path = f"{out_dir}/{file}"
    if "--check" in sys.argv:
        assert os.path.exists(path) and open(path, "rb").read() == content, f"{path} is out of date"
    else:
        open(path, "wb").write(content)
print(f"tzdata {version}: {len(order)} names, {len(offset_of)} zones, tzdata.bin {len(bin_out)} B, windows.txt {len(rows)} rows {len(txt_out)} B")

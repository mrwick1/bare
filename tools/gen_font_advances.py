#!/usr/bin/env python3
"""Generate crates/layout/src/advances.rs: the advance width of each letter of Noto Sans Regular and Bold.

Run from the repo root: python3 tools/gen_font_advances.py

Sources: data/fonts/NotoSans-Regular-subset.ttf and data/fonts/NotoSans-Bold-subset.ttf (license in data/fonts/OFL-Sans.txt).
The font has 1000 units to the em, so an advance in `hmtx` is already in 1/1000 em, the unit of `bare_layout::font::Units`.
The test `the_advance_tables_match_the_font_files` in crates/paint/src/glyphs.rs checks the output against the font files with another reader (ab_glyph).
"""

import struct

FONTS = [("REGULAR", "data/fonts/NotoSans-Regular-subset.ttf"), ("BOLD", "data/fonts/NotoSans-Bold-subset.ttf")]
OUT = "crates/layout/src/advances.rs"


def tables(data):
    """The tables of a TrueType file: name -> (offset, length)."""
    count = struct.unpack_from(">H", data, 4)[0]
    return {data[12 + 16 * i : 16 + 16 * i].decode(): struct.unpack_from(">II", data, 20 + 16 * i) for i in range(count)}


def cmap(data, at):
    """code point -> glyph, from the Unicode subtables (format 4 and format 12) of `cmap`."""
    count = struct.unpack_from(">H", data, at + 2)[0]
    glyphs = {}
    for i in range(count):
        platform, encoding, offset = struct.unpack_from(">HHI", data, at + 4 + 8 * i)
        if (platform, encoding) not in [(0, 3), (0, 4), (3, 1), (3, 10)]:
            continue
        sub = at + offset
        fmt = struct.unpack_from(">H", data, sub)[0]
        if fmt == 4:
            segs = struct.unpack_from(">H", data, sub + 6)[0] // 2
            ends = struct.unpack_from(f">{segs}H", data, sub + 14)
            starts = struct.unpack_from(f">{segs}H", data, sub + 16 + 2 * segs)
            deltas = struct.unpack_from(f">{segs}h", data, sub + 16 + 4 * segs)
            range_at = sub + 16 + 6 * segs
            offsets = struct.unpack_from(f">{segs}H", data, range_at)
            for s in range(segs):
                for cp in range(starts[s], ends[s] + 1):
                    if cp == 0xFFFF:
                        continue
                    if offsets[s] == 0:
                        g = (cp + deltas[s]) & 0xFFFF
                    else:
                        g = struct.unpack_from(">H", data, range_at + 2 * s + offsets[s] + 2 * (cp - starts[s]))[0]
                        g = (g + deltas[s]) & 0xFFFF if g else 0
                    if g:
                        glyphs[cp] = g
        elif fmt == 12:
            groups = struct.unpack_from(">I", data, sub + 12)[0]
            for g in range(groups):
                first, last, glyph = struct.unpack_from(">III", data, sub + 16 + 12 * g)
                for cp in range(first, last + 1):
                    glyphs[cp] = glyph + cp - first
    return glyphs


def advances(path):
    """code point -> advance in 1/1000 em, sorted by code point."""
    data = open(path, "rb").read()
    t = tables(data)
    units = struct.unpack_from(">H", data, t["head"][0] + 18)[0]
    assert units == 1000, f"{path}: unitsPerEm is {units}, not 1000"
    metrics = struct.unpack_from(">H", data, t["hhea"][0] + 34)[0]
    hmtx = t["hmtx"][0]

    def advance(glyph):
        return struct.unpack_from(">H", data, hmtx + 4 * min(glyph, metrics - 1))[0]

    return {cp: advance(g) for cp, g in sorted(cmap(data, t["cmap"][0]).items())}


def table(name, adv):
    """The advances of ASCII as an array, and the others as pairs sorted by code point (a binary search)."""
    ascii = [adv.get(cp, 600) for cp in range(128)]
    rest = [(cp, a) for cp, a in adv.items() if cp >= 128]
    out = [f"/// Advances of the letters 0 to 127, in 1/1000 em. 600 (0.6 em, the width of Noto Sans Mono) for a letter that the font does not have.", f"pub static {name}_ASCII: [u16; 128] = ["]
    for i in range(0, 128, 16):
        out.append("    " + ", ".join(str(a) for a in ascii[i : i + 16]) + ",")
    out.append("];")
    out.append(f"/// The other letters of the font: (code point, advance in 1/1000 em), sorted by code point.")
    out.append(f"pub static {name}: [(u32, u16); {len(rest)}] = [")
    for i in range(0, len(rest), 6):
        out.append("    " + ", ".join(f"(0x{cp:x}, {a})" for cp, a in rest[i : i + 6]) + ",")
    out.append("];")
    return "\n".join(out)


header = """//! Advance widths of Noto Sans Regular and Bold, from `hmtx`, in 1/1000 em. Made by `tools/gen_font_advances.py`: do not edit.
//! `font::width_of` reads these tables. The letters that the font lacks are not here.

"""
with open(OUT, "w") as f:
    f.write(header + "\n\n".join(table(name, advances(path)) for name, path in FONTS) + "\n")
print(f"wrote {OUT}")

#!/usr/bin/env python3
"""Generate crates/unicode/src/tables.rs from the Unicode data files in data/unicode/.

Run from the repo root: python3 tools/gen_unicode_tables.py

Sources (Unicode 18.0.0, license in data/unicode/LICENSE.txt):
  https://www.unicode.org/Public/idna/latest/IdnaMappingTable.txt
  https://www.unicode.org/Public/idna/latest/IdnaTestV2.txt
  https://www.unicode.org/Public/UCD/latest/ucd/UnicodeData.txt
  https://www.unicode.org/Public/UCD/latest/ucd/CompositionExclusions.txt
  https://www.unicode.org/Public/UCD/latest/ucd/extracted/DerivedBidiClass.txt
  https://www.unicode.org/Public/UCD/latest/ucd/extracted/DerivedJoiningType.txt
  https://www.unicode.org/Public/UCD/latest/ucd/CaseFolding.txt

To update Unicode: download the new files over the old ones, then run this script.
"""

import os
import re

DATA = "data/unicode"
OUT = "crates/unicode/src/tables.rs"


def rows(name):
    """Yield (first, last, fields) for each data line. Comments and blank lines are skipped."""
    with open(os.path.join(DATA, name), encoding="utf-8") as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if not line:
                continue
            fields = [x.strip() for x in line.split(";")]
            first, _, last = fields[0].partition("..")
            yield int(first, 16), int(last or first, 16), fields[1:]


def missing(name):
    """Yield (first, last, value) for each '# @missing' default line, in file order."""
    with open(os.path.join(DATA, name), encoding="utf-8") as f:
        for line in f:
            m = re.match(r"# @missing: ([0-9A-F]+)\.\.([0-9A-F]+); (\w+)", line)
            if m:
                yield int(m[1], 16), int(m[2], 16), m[3]


def merge(ranges):
    """Join neighbouring ranges that have the same value."""
    out = []
    for first, last, value in sorted(ranges):
        if out and out[-1][1] + 1 == first and out[-1][2] == value:
            out[-1] = (out[-1][0], last, value)
        else:
            out.append((first, last, value))
    return out


def rust_str(cps):
    return '"' + "".join(f"\\u{{{cp:x}}}" for cp in cps) + '"'


# UnicodeData.txt: combining class, general category, canonical decompositions.
ccc, marks, decomp, compat = {}, [], {}, {}
for cp, _, f in rows("UnicodeData.txt"):
    category, combining, mapping = f[1], int(f[2]), f[4]
    if combining:
        ccc[cp] = combining
    if category.startswith("M"):
        marks.append((cp, cp, "()"))
    if mapping.startswith("<"):
        compat[cp] = [int(x, 16) for x in mapping.split()[1:]]
    elif mapping:
        decomp[cp] = [int(x, 16) for x in mapping.split()]

# Full canonical decomposition: apply the mappings until nothing changes.
def full(cp):
    return [y for x in decomp[cp] for y in (full(x) if x in decomp else [x])]

full_decomp = {cp: full(cp) for cp in decomp}

# Compatibility decomposition also expands canonical mappings and Korean syllables.
def full_compat(cp):
    if 0xAC00 <= cp < 0xD7A4:
        s = cp - 0xAC00
        parts = [0x1100 + s // 588, 0x1161 + (s // 28) % 21]
        if s % 28:
            parts.append(0x11A7 + s % 28)
        return parts
    mapping = compat.get(cp, decomp.get(cp))
    return [cp] if mapping is None else [y for x in mapping for y in full_compat(x)]

full_compat_decomp = {}
for cp in compat.keys() | decomp.keys():
    parts = full_compat(cp)
    if parts != full_decomp.get(cp, [cp]):
        full_compat_decomp[cp] = parts

# Composition pairs. Full_Composition_Exclusion = the exclusion list, plus singletons,
# plus non-starter decompositions (UAX #15).
excluded = {cp for cp, _, _ in rows("CompositionExclusions.txt")}
compose = []
for cp, parts in decomp.items():
    if cp in excluded or len(parts) == 1 or ccc.get(cp, 0) or ccc.get(parts[0], 0):
        continue
    compose.append((parts[0], parts[1], cp))

# IDNA mapping table.
STATUS = {"valid": "Valid", "ignored": "Ignored", "mapped": "Mapped", "deviation": "Deviation", "disallowed": "Disallowed"}
idna = []
for first, last, f in rows("IdnaMappingTable.txt"):
    mapping = [int(x, 16) for x in f[1].split()] if len(f) > 1 and f[1] else []
    idna.append((first, last, f"(Idna::{STATUS[f[0]]}, {rust_str(mapping)})"))

# Bidi classes. Defaults first (@missing), then the explicit values on top.
BIDI = {"L": "L", "R": "R", "AL": "AL", "AN": "AN", "EN": "EN", "ES": "ES", "CS": "CS", "ET": "ET",
        "ON": "ON", "BN": "BN", "NSM": "NSM", "Left_To_Right": "L", "Right_To_Left": "R",
        "Arabic_Letter": "AL", "European_Terminator": "ET", "Boundary_Neutral": "BN"}
bidi = {}
for first, last, value in missing("DerivedBidiClass.txt"):
    for cp in range(first, last + 1):
        bidi[cp] = BIDI.get(value, "Other")
for first, last, f in rows("DerivedBidiClass.txt"):
    for cp in range(first, last + 1):
        bidi[cp] = BIDI.get(f[0], "Other")
bidi_ranges = merge((cp, cp, f"Bidi::{v}") for cp, v in bidi.items() if v != "L")

# Joining types. The default, Non_Joining (U), is left out.
joining = merge((first, last, f"Joining::{f[0]}") for first, last, f in rows("DerivedJoiningType.txt"))

# Simple case folding: statuses C (common) and S (simple). F and T are full or Turkic mappings.
fold = sorted((first, int(f[1], 16)) for first, _, f in rows("CaseFolding.txt") if f[0] in ("C", "S"))


def table(name, kind, items):
    lines = [f"pub static {name}: &[{kind}] = &["]
    lines += [f"    {item}," for item in items]
    return "\n".join(lines) + "\n];\n"


def ranges(name, kind, items):
    return table(name, f"(u32, u32, {kind})", (f"(0x{a:x}, 0x{b:x}, {v})" for a, b, v in items))


with open(OUT, "w", encoding="utf-8") as out:
    out.write("""\
//! Unicode data tables for IDNA, NFC and case folding.
//!
//! Generated by `tools/gen_unicode_tables.py` from the Unicode 18.0.0 data files in
//! `data/unicode/`. Do not edit by hand. Each range table is sorted, so it can be binary searched.

/// Status of a code point in the IDNA mapping table (UTS #46, section 5).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Idna {
    Valid,
    Ignored,
    Mapped,
    Deviation,
    Disallowed,
}

/// Bidi class (UAX #9). Only the classes that the IDNA bidi rules use have their own value.
/// `L` is the default and is not stored in the table.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Bidi {
    L,
    R,
    AL,
    AN,
    EN,
    ES,
    CS,
    ET,
    ON,
    BN,
    NSM,
    Other,
}

/// Joining type (Arabic shaping). Non_Joining is the default and is not stored in the table.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Joining {
    C,
    D,
    L,
    R,
    T,
}

""")
    out.write(ranges("IDNA", "(Idna, &str)", idna) + "\n")
    out.write(ranges("BIDI", "Bidi", bidi_ranges) + "\n")
    out.write(ranges("JOINING", "Joining", joining) + "\n")
    out.write(ranges("COMBINING_CLASS", "u8", merge((cp, cp, v) for cp, v in ccc.items())) + "\n")
    out.write(ranges("MARKS", "()", merge(marks)) + "\n")
    # DECOMPOSE and DECOMPOSE_DATA are arrays, not slices or &str. They hold no pointer, so the loader has nothing to
    # relocate at process start. Each entry is (code point, first index in DECOMPOSE_DATA, length).
    data, entries = [], []
    for cp, parts in sorted(full_decomp.items()):
        assert len(parts) < 256 and len(data) < 65536
        entries.append(f"    (0x{cp:x}, {len(data)}, {len(parts)}),\n")
        data += parts
    out.write("/// Full canonical decomposition: (code point, first index in `DECOMPOSE_DATA`, length).\n")
    out.write(f"pub static DECOMPOSE: [(u32, u16, u8); {len(entries)}] = [\n{''.join(entries)}];\n\n")
    out.write("/// The decomposed parts of all `DECOMPOSE` entries, one entry after the other.\n")
    out.write(f"pub static DECOMPOSE_DATA: [char; {len(data)}] = [\n")
    for i in range(0, len(data), 8):
        out.write("    " + ", ".join(f"'\\u{{{c:x}}}'" for c in data[i:i + 8]) + ",\n")
    out.write("];\n\n")
    # Extra mappings for NFKD. Keep the data free of pointers, as for DECOMPOSE.
    data, entries = [], []
    for cp, parts in sorted(full_compat_decomp.items()):
        assert len(parts) < 256 and len(data) < 65536
        entries.append(f"    (0x{cp:x}, {len(data)}, {len(parts)}),\n")
        data += parts
    out.write("/// Compatibility decomposition when it differs from the canonical one.\n")
    out.write(f"pub static COMPAT_DECOMPOSE: [(u32, u16, u8); {len(entries)}] = [\n{''.join(entries)}];\n\n")
    out.write(f"pub static COMPAT_DECOMPOSE_DATA: [char; {len(data)}] = [\n")
    for i in range(0, len(data), 8):
        out.write("    " + ", ".join(f"'\\u{{{c:x}}}'" for c in data[i:i + 8]) + ",\n")
    out.write("];\n\n")
    out.write(table("COMPOSE", "(u32, u32, u32)", (f"(0x{a:x}, 0x{b:x}, 0x{c:x})" for a, b, c in sorted(compose))) + "\n")
    out.write("/// Simple case folding (CaseFolding.txt, statuses C and S): (code point, folded code point).\n")
    out.write(table("CASE_FOLD", "(u32, u32)", (f"(0x{a:x}, 0x{b:x})" for a, b in fold)) + "\n")
    out.write("/// The same pairs as `CASE_FOLD`, reversed and sorted by folding: (folded code point, code point).\n")
    out.write(table("CASE_FOLD_INV", "(u32, u32)", (f"(0x{f:x}, 0x{c:x})" for f, c in sorted((f, c) for c, f in fold))))

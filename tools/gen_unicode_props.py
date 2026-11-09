#!/usr/bin/env python3
"""Generate crates/unicode/src/props.rs: the tables of RegExp `\\p{...}` (ECMA-262 UnicodeMatchProperty).

Run from the repo root: python3 tools/gen_unicode_props.py

Sources (Unicode 18.0.0, in data/unicode/, license in LICENSE.txt):
  UnicodeData.txt (General_Category, Bidi_Mirrored), Scripts.txt, ScriptExtensions.txt, PropList.txt,
  DerivedCoreProperties.txt, DerivedNormalizationProps.txt, emoji/emoji-data.txt (as emoji-data.txt), PropertyAliases.txt, PropertyValueAliases.txt.
Output: static, sorted tables. Ranges are `(first, last, value)`. The names are sorted so that a lookup is a binary search.
"""
import os

D = "data/unicode/"
MAX = 0x10FFFF

# The binary properties that ECMA-262 allows (table 70), except `Any`, `ASCII` and `Assigned`, which we make here.
BINARY = """ASCII_Hex_Digit Alphabetic Bidi_Control Bidi_Mirrored Case_Ignorable Cased Changes_When_Casefolded Changes_When_Casemapped
Changes_When_Lowercased Changes_When_NFKC_Casefolded Changes_When_Titlecased Changes_When_Uppercased Dash Default_Ignorable_Code_Point
Deprecated Diacritic Emoji Emoji_Component Emoji_Modifier Emoji_Modifier_Base Emoji_Presentation Extended_Pictographic Extender
Grapheme_Base Grapheme_Extend Hex_Digit IDS_Binary_Operator IDS_Trinary_Operator ID_Continue ID_Start Ideographic Join_Control
Logical_Order_Exception Lowercase Math Noncharacter_Code_Point Pattern_Syntax Pattern_White_Space Quotation_Mark Radical
Regional_Indicator Sentence_Terminal Soft_Dotted Terminal_Punctuation Unified_Ideograph Uppercase Variation_Selector White_Space
XID_Continue XID_Start""".split()

GC_GROUPS = {
    "L": ["Lu", "Ll", "Lt", "Lm", "Lo"],
    "LC": ["Lu", "Ll", "Lt"],
    "M": ["Mn", "Mc", "Me"],
    "N": ["Nd", "Nl", "No"],
    "P": ["Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po"],
    "S": ["Sm", "Sc", "Sk", "So"],
    "Z": ["Zs", "Zl", "Zp"],
    "C": ["Cc", "Cf", "Cs", "Co", "Cn"],
}


def lines(name):
    with open(D + name, encoding="utf-8") as f:
        for line in f:
            line = line.split("#", 1)[0].strip()
            if line:
                yield [x.strip() for x in line.split(";")]


def span(text):
    a, _, b = text.partition("..")
    return int(a, 16), int(b or a, 16)


def merge(ranges):
    out = []
    for a, b in sorted(ranges):
        if out and a <= out[-1][1] + 1:
            out[-1][1] = max(out[-1][1], b)
        else:
            out.append([a, b])
    return [(a, b) for a, b in out]


def rs_str(s):
    return '"' + s + '"'


# --- General_Category --------------------------------------------------------------------------------------------------------------
gc_rows = []  # (first, last, code)
bidi_mirrored = []
first = None
for f in lines("UnicodeData.txt"):
    cp, name, cat, mirrored = int(f[0], 16), f[1], f[2], f[9]
    if name.endswith(", First>"):
        first = cp
        continue
    lo = first if name.endswith(", Last>") else cp
    first = None
    gc_rows.append((lo, cp, cat))
    if mirrored == "Y":
        bidi_mirrored.append((lo, cp))
gc_rows.sort()
# Fill the gaps with Cn.
full, nxt = [], 0
for a, b, c in gc_rows:
    if a > nxt:
        full.append((nxt, a - 1, "Cn"))
    full.append((a, b, c))
    nxt = b + 1
if nxt <= MAX:
    full.append((nxt, MAX, "Cn"))
merged = []
for a, b, c in full:
    if merged and merged[-1][2] == c and merged[-1][1] + 1 == a:
        merged[-1] = (merged[-1][0], b, c)
    else:
        merged.append((a, b, c))
gc_codes = sorted({c for _, _, c in merged})
assert len(gc_codes) == 30, gc_codes
gc_index = {c: i for i, c in enumerate(gc_codes)}

# The names of the values: every name that PropertyValueAliases.txt lists for the code, with the groups.
gc_names = {}  # name -> mask
for f in lines("PropertyValueAliases.txt"):
    if f[0] == "gc":
        names = f[1:]
        code = names[0]
        mask = sum(1 << gc_index[c] for c in GC_GROUPS.get(code, [code]))
        for n in names:
            gc_names[n] = mask

# --- Scripts -----------------------------------------------------------------------------------------------------------------------
sc_alias = {}  # every name -> the long name
for f in lines("PropertyValueAliases.txt"):
    if f[0] == "sc":
        for n in f[1:]:
            sc_alias[n] = f[2]
sc_long = sorted(set(sc_alias.values()))
assert len(sc_long) < 255
sc_id = {n: i for i, n in enumerate(sc_long)}
sc_rows = []
for f in lines("Scripts.txt"):
    a, b = span(f[0])
    sc_rows.append((a, b, sc_id[f[1]]))
sc_rows.sort()
# Cn code points have script Unknown (Zzzz) in the standard. Fill the gaps.
filled, nxt = [], 0
for a, b, s in sc_rows:
    if a > nxt:
        filled.append((nxt, a - 1, sc_id["Unknown"]))
    filled.append((a, b, s))
    nxt = b + 1
if nxt <= MAX:
    filled.append((nxt, MAX, sc_id["Unknown"]))
sc_rows = []
for a, b, s in filled:
    if sc_rows and sc_rows[-1][2] == s and sc_rows[-1][1] + 1 == a:
        sc_rows[-1] = (sc_rows[-1][0], b, s)
    else:
        sc_rows.append((a, b, s))

# Script_Extensions: the listed ranges have a set of scripts; the rest have their Script.
scx_sets, scx_rows = [], []
for f in lines("ScriptExtensions.txt"):
    a, b = span(f[0])
    ids = tuple(sorted(sc_id[sc_alias[n]] for n in f[1].split()))
    if ids not in scx_sets:
        scx_sets.append(ids)
    scx_rows.append((a, b, scx_sets.index(ids)))
scx_rows.sort()

# --- Binary properties -------------------------------------------------------------------------------------------------------------
bin_ranges = {}
for name in ("PropList.txt", "DerivedCoreProperties.txt", "DerivedNormalizationProps.txt", "emoji-data.txt"):
    for f in lines(name):
        if len(f) == 2 and f[1] in BINARY:
            bin_ranges.setdefault(f[1], []).append(span(f[0]))
bin_ranges["Bidi_Mirrored"] = bidi_mirrored
missing = [n for n in BINARY if n not in bin_ranges]
assert not missing, missing
# Every name of a property: from PropertyAliases.txt.
bin_names = {}
for f in lines("PropertyAliases.txt"):
    for n in f:
        if n in bin_ranges:
            for m in f:
                bin_names[m] = n
            break
for n in BINARY:
    bin_names.setdefault(n, n)

# --- Write -------------------------------------------------------------------------------------------------------------------------
out = []
w = out.append
w("//! Unicode property tables for RegExp `\\p{...}`. Generated by tools/gen_unicode_props.py (Unicode 18.0.0). Do not edit.")
w("//! All tables are sorted: the ranges by code point, the names by byte order, so that a lookup is a binary search.")
w("")
w("/// The General_Category of every code point: `(first, last, index in GC_CODES)`.")
w("pub static GC: &[(u32, u32, u8)] = &[")
for a, b, c in merged:
    w(f"    ({a:#x}, {b:#x}, {gc_index[c]}),")
w("];")
w("")
w("pub static GC_CODES: &[&str] = &[" + ", ".join(rs_str(c) for c in gc_codes) + "];")
w("")
w("/// The names of General_Category values (`L`, `Letter`, `Lu`, ...) with the set of codes: bit `i` is `GC_CODES[i]`.")
w("pub static GC_NAMES: &[(&str, u32)] = &[")
for n in sorted(gc_names):
    w(f"    ({rs_str(n)}, {gc_names[n]:#x}),")
w("];")
w("")
w("/// The Script of every code point: `(first, last, index in SCRIPT_LONG)`.")
w("pub static SCRIPT: &[(u32, u32, u8)] = &[")
for a, b, s in sc_rows:
    w(f"    ({a:#x}, {b:#x}, {s}),")
w("];")
w("")
w("pub static SCRIPT_LONG: &[&str] = &[" + ", ".join(rs_str(n) for n in sc_long) + "];")
w("")
w("/// The names of scripts (long and short) with the index in SCRIPT_LONG.")
w("pub static SCRIPT_NAMES: &[(&str, u8)] = &[")
for n in sorted(sc_alias):
    w(f"    ({rs_str(n)}, {sc_id[sc_alias[n]]}),")
w("];")
w("")
w("/// Script_Extensions of the code points that have more than their Script: `(first, last, index in SCX_SETS)`.")
w("pub static SCX: &[(u32, u32, u16)] = &[")
for a, b, i in scx_rows:
    w(f"    ({a:#x}, {b:#x}, {i}),")
w("];")
w("")
w("/// Each set is a sorted list of indexes in SCRIPT_LONG.")
w("pub static SCX_SETS: &[&[u8]] = &[")
for s in scx_sets:
    w("    &[" + ", ".join(map(str, s)) + "],")
w("];")
w("")
w("/// The binary properties: each range list is sorted and has no touching ranges.")
w("pub static BINARY_RANGES: &[&[(u32, u32)]] = &[")
order = sorted(bin_ranges)
for n in order:
    w(f"    // {n}")
    w("    &[" + ", ".join(f"({a:#x}, {b:#x})" for a, b in merge(bin_ranges[n])) + "],")
w("];")
w("")
w("/// The names of binary properties (long and short) with the index in BINARY_RANGES.")
w("pub static BINARY_NAMES: &[(&str, u8)] = &[")
for n in sorted(bin_names):
    w(f"    ({rs_str(n)}, {order.index(bin_names[n])}),")
w("];")
w("")
with open("crates/unicode/src/props.rs", "w") as f:
    f.write("\n".join(out))
print("props.rs:", os.path.getsize("crates/unicode/src/props.rs"), "bytes;", len(merged), "gc ranges,", len(sc_rows), "script ranges,", len(scx_rows), "scx ranges")

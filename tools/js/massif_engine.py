#!/usr/bin/env python3
"""The heap bytes of the engine in a massif output, at the snapshot where they peak (plan step M3, tools/js/layout_census.sh).

  massif_engine.py MASSIF_OUT [--host-parse] [--top N]

Massif lists each allocation with its call stack. An allocation belongs to the engine when the frame nearest to the allocator that
names a crate is a `bare_js` frame; the rest is the host (the example or the browser). With --host-parse, a tree that the caller
parsed itself (a `parse_script*` frame with no `Interp::` frame) is also the host's: the examples keep their own copy of the tree
after `Interp::new` clones it. Prints one line: the total heap of the peak snapshot, the engine bytes and the host bytes.
"""
import collections
import re
import sys

CRATE = re.compile(r"^<?([a-z][a-z0-9_]*)::")
SKIP = ("core", "alloc", "std", "hashbrown")


def snapshots(path):
    snaps, cur, intree = [], None, False
    for line in open(path):
        if line.startswith("snapshot="):
            cur = {"tree": [], "heap": 0}
            snaps.append(cur)
            intree = False
        elif cur is not None:
            if line.startswith("mem_heap_B="):
                cur["heap"] = int(line.split("=")[1])
            elif line.startswith("heap_tree="):
                intree = not line.strip().endswith("empty")
            elif intree and line.lstrip().startswith("n"):
                cur["tree"].append(line.rstrip("\n"))
    return [s for s in snaps if s["tree"]]


def classify(tree, host_parse):
    """Bytes by (owner, nearest crate frame). A node of the tree is a frame; its children are its callers."""
    nodes = []
    for line in tree:
        m = re.match(r"\s*n(\d+): (\d+) (.*)", line)
        nodes.append((int(m.group(1)), int(m.group(2)), m.group(3)))
    out, pos = collections.Counter(), [0]

    def walk(path):
        n, b, d = nodes[pos[0]]
        pos[0] += 1
        path = path + [re.sub(r"^0x[0-9A-Fa-f]+: ", "", d)]
        below = sum(walk(path) for _ in range(n))
        own = b - below
        if own > 0:
            crate = next((m.group(1) for x in path if (m := CRATE.match(x)) and m.group(1) not in SKIP), None)
            frames = " ".join(path)
            owner = "engine" if crate == "bare_js" else "host"
            if host_parse and re.search(r"parse_script\w*", frames) and "Interp::" not in frames:
                owner = "host"
            out[(owner, next((re.sub(r" \(.*", "", x)[:80] for x in path if CRATE.match(x)), "?"))] += own
        return b

    sys.setrecursionlimit(1 << 16)
    walk([])
    return out


def main():
    args = sys.argv[1:]
    host_parse = "--host-parse" in args
    top = int(args[args.index("--top") + 1]) if "--top" in args else 0
    path = next(a for a in args if not a.startswith("--") and not a.isdigit())
    best = None
    for s in snapshots(path):
        by = classify(s["tree"], host_parse)
        engine = sum(v for (o, _), v in by.items() if o == "engine")
        if best is None or engine > best[0]:
            best = (engine, s["heap"], by)
    engine, heap, by = best
    print(f"massif peak_heap={heap} engine={engine} host={heap - engine}")
    for (o, f), v in sorted(by.items(), key=lambda t: -t[1])[:top]:
        print(f"  {v:>10} {o:6} {f}")


main()

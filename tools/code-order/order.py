#!/usr/bin/env python3
"""Generate an LLD order from Callgrind self costs, or check an existing order."""

import argparse
from bisect import bisect_right
from collections import defaultdict
from pathlib import Path
import subprocess


def text_symbols(binary):
    output = subprocess.check_output(
        ["nm", "-n", "-S", "--defined-only", binary], text=True
    )
    symbols = []
    for line in output.splitlines():
        fields = line.split(maxsplit=3)
        if len(fields) == 4 and fields[2] in ("T", "t", "W", "w"):
            address, size, _, name = fields
            if int(size, 16):
                symbols.append((int(address, 16), int(size, 16), name))
    if not symbols:
        raise ValueError(f"{binary}: no sized text symbols. Build with --symbols.")
    return symbols


def compiler_version(binary):
    output = subprocess.check_output(["readelf", "-p", ".comment", binary], text=True)
    versions = {
        line.split("rustc version ", 1)[1]
        for line in output.splitlines() if "rustc version " in line
    }
    if len(versions) != 1:
        raise ValueError(f"{binary}: expected one Rust compiler version in .comment")
    return "rustc " + versions.pop()


def executed_symbols(profile, binary, symbols):
    starts = [address for address, _, _ in symbols]
    executed = set()
    in_binary = False
    call_cost = False
    with profile.open() as stream:
        for line in stream:
            if line.startswith("ob="):
                in_binary = Path(line[3:].strip()) == binary
            elif line.startswith("calls="):
                call_cost = True
            elif line.startswith("0x"):
                if in_binary and not call_cost:
                    address, _, cost = line.split()
                    pc = int(address, 16)
                    index = bisect_right(starts, pc) - 1
                    if int(cost) and index >= 0:
                        start, size, name = symbols[index]
                        if pc < start + size:
                            executed.add(name)
                call_cost = False
    if not executed:
        raise ValueError(f"{profile}: no matching self costs for {binary}")
    return executed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("profiles", nargs="*", type=Path, help="startup profile first")
    parser.add_argument("--check", type=Path, help="count stale entries in this order")
    args = parser.parse_args()
    binary = args.binary.resolve()
    symbols = text_symbols(binary)
    names = {name: address for address, _, name in symbols}
    if args.check:
        if args.profiles:
            parser.error("--check takes no profiles")
        order = [
            line for line in args.check.read_text().splitlines()
            if line and not line.startswith("#")
        ]
        present = len(set(order) & names.keys())
        print(f"{present}/{len(order)} symbols present in {binary.name}")
        return
    if not args.profiles:
        parser.error("give one or more Callgrind profiles")

    membership = defaultdict(list)
    for index, profile in enumerate(args.profiles):
        for name in executed_symbols(profile, binary, symbols):
            membership[name].append(index)

    # Group startup first, then by how many workloads share the function.
    # Keep functions with the same workload set together, in input address order.
    def rank(name):
        workloads = membership[name]
        return (workloads[0] != 0, -len(workloads), workloads, names[name], name)

    print(f"# {compiler_version(binary)}")
    print(*sorted(membership, key=rank), sep="\n")


if __name__ == "__main__":
    main()

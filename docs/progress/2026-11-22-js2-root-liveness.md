# Verify js2 root-map coverage

## Design

Decode the instructions once. Use the operand table to identify register reads and writes. Track the first matching handler and the normal successors of each instruction. A write on the success path does not kill the old value on a path that throws. A resume defines its sent value and mode and restores its saved registers.

Propagate register bitsets backward along the control-flow edges. Compare the resulting live registers with the maps that the collector reads before the instruction writes its result. For I instructions, E edges, and W words per bitset, storage is O(IW + E). Each propagation step costs O(W) per incoming edge. At most the register count of new bits can reach each instruction. The design adds no fields to the bytecode or root maps.

Block summaries can reduce storage at the cost of a second scan over the instructions to check each safepoint. A stored proof could permit one validation pass, but would add data to the cache file. V8's optimizing compiler computes bytecode liveness in reverse order and revisits loop back edges separately. See [V8 bytecode analysis](https://chromium.googlesource.com/v8/v8/+/5716fe8de7cfac4b583d3c2ab4499083f3e22ecc/src/compiler/bytecode-analysis.cc).

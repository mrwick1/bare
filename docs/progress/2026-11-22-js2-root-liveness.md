# Verify js2 root-map coverage

## Design

Decode the instructions once. Use the operand table to identify register reads and writes. Track the first matching handler and the normal successors of each instruction. A write on the success path does not kill the old value on a path that throws. A resume defines its sent value and mode and restores its saved registers.

Propagate register bitsets backward along the control-flow edges. Compare the resulting live registers with the maps that the collector reads before the instruction writes its result. For I instructions, E edges, and W words per bitset, storage is O(IW + E). Each propagation step costs O(W) per incoming edge. At most the register count of new bits can reach each instruction. The design adds no fields to the bytecode or root maps.

Block summaries can reduce storage at the cost of a second scan over the instructions to check each safepoint. A stored proof could permit one validation pass, but would add data to the cache file. V8's optimizing compiler computes bytecode liveness in reverse order and revisits loop back edges separately. See [V8 bytecode analysis](https://chromium.googlesource.com/v8/v8/+/5716fe8de7cfac4b583d3c2ab4499083f3e22ecc/src/compiler/bytecode-analysis.cc).

## Compiler changes

- Mark the old numeric value live after ToNumeric in a postfix update. With collection forced, the regression test returned the incremented BigInt before the fix and returned the old BigInt after it.
- Mark the result of a compound member assignment before the store. The chained form `a.x = a.y.z += a.w` can allocate a result that the first store must keep.
- Initialize the async `yield*` result slot on every loop entry. The no-throw branch of AYsCall leaves this slot untouched. A resume does not restore the earlier result.

## Metadata checks

- Check local function and recipe operands as before. Check resolved child indexes against the code table in `verify_program`.
- Reject a recipe that serves as both an object key list and a template. Check template pairs and allow the undefined sentinel only in cooked entries.
- Check the name atom, generator kind, and save area size. Require both resume registers for an async generator's Yield.
- Check that Lazy is the only word of its stub, its argument mode agrees with its flags and frame, and the stub has no executable tables or maps. The loader owns the source and pending compilation entries.

## Existing verifier fixtures

- `a_valid_function_passes` adds r1 to the shared loop maps and rejects the former incomplete map at the loop header.
- `source_positions` uses the same loop maps with r1.
- `handlers` adds r1 at Add.
- `reads_come_after_writes` adds the iterator record r1 through r3 after IterOpen.

## Instruction cost

Benchmarked release builds on archMachine, an AMD Ryzen 9 5900HX. The base was `14237fe1` and the candidate was `3d2a7bd2`. The React fixture was `/data/bare-tmp/r22/gap-webpack/react-app.js`, 314,192 bytes, 1,532 functions and 66,143 bytecode words. Its SHA-256 starts with `1992c72031ad2bad4`.

Counted user-mode instructions with `perf stat`. Each process first loaded and compiled the fixture. The zero-repeat case measured that setup. The table subtracts setup and reports instructions per verification or eager compile. Each case ran in three processes. The first check ran once per process. Repeated checks ran 30 times in each process.

| Case | Verifier instructions | Compile instructions | Verifier share |
| --- | ---: | ---: | ---: |
| First check | 80.901–80.930 M | 316.083–316.159 M | 25.60% |
| Repeated | 80.882–80.890 M | 315.479–315.489 M | 25.64% |

The first implementation needed 210.7 M instructions per repeated check, or 67.7% of eager compile. Reusing operand effects and storing data flow in flat arrays reduced this to 93.0 M. Reusing the maps' bitsets in both passes reduced it to 80.9 M. The base verifier needed 140.0 M instructions and checked fewer properties.

The five `interp_load` rows used 100,000 turns, except `fib 24`. With three runs per row and allocator, the mean change was -0.026% on glibc and -0.025% on jemalloc. The four `cost_matrix.sh` js2 rows averaged -0.007% and -0.003%. No row exceeded +0.5% under either allocator.

## Cost debt

The repeated verifier run was 0.64 percentage points above an exact 25% comparison. The verifier has no disk-load caller yet, so this ratio measures verification in memory.

## Tests

- Linux and Windows VM. Verifier 28, compiler 93, interpreter 38, script 78. All passed.
- Linux package check passed. The forced-collection postfix test passed under Miri.
- Test262 compound assignment and postfix increment/decrement. The base and candidate each passed 456 of 530. No lost passes.
- The workspace suite, peak memory, and disk cache loads were not tested.

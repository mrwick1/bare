# js2 disk code cache: paused (owner, 2026-11-22)

## Decision

The owner paused the js2 disk code cache on 2026-11-22. The rule for it stays: a unit read from disk must pass a check that proves the same contract as compiler output. A checksum or a signed file is not enough.

## Why

- #913 made the verifier prove complete root maps. On the 314,192-byte React fixture, verification costs 25.6% of an eager compile.
- A design pass for the cache found three more parts of the contract that only compiler output meets: closure-context slot bounds, the range of prelude slot operands, and the facts that the lazy compiler reads.
- A prototype checked all three. The lazy-facts check spends 100.08% of a cold lazy compile (75.76 M against 75.70 M instructions, 3 runs, archMachine). It must parse the source again to prove token boundaries, skipped bodies and captures. So a warm disk load saves no CPU.
- In-session sharing (#910) already removes the parse and compile of a repeated bundle in later documents (76.2 M to 0.49 M instructions per extra realm). The HTTP disk cache (#912) removes the download in a later run.

## Saved work

- Context and prelude-slot checks, facts check and tests: kept outside the repo as a diff (owner session). They were not merged.
- Compiler bug found by the prototype: a body function named `arguments` shares a binding with the arguments object of parameters with expressions. A separate fix PR follows.

## Options for later

- Reuse facts from a local parse of the same source, goal and limits. This does not help a cold process.
- A cheaper proof format for lazy facts that a check can confirm in one linear pass, without a parse.

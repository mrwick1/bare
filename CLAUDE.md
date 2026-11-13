# CLAUDE.md — bare

## Main focus

Performance and resource use come first: CPU instructions, peak memory and time to ready. Weigh every change against them.

## How to improve a part

Do not patch hot spots one by one. A 10% gain on one benchmark row is not the goal. Look for a change in design that removes a whole kind of work.

1. Measure first. Profile the whole browser on the public journeys and the load-test ramps. Use a build with symbols. Split the cost by subsystem: network, parsing, style, layout, paint and JS. Work on the subsystem with the largest share.
2. Study the design, not the symptom. Write down the current data structures and algorithms, and their time and memory complexity. Read how real engines solve the same problem (Chrome, Firefox and Servo, WebKit, Ladybird) and what research gives.
3. Compare options. List at least two other designs. For each, give the data structures, the algorithm, the complexity, the memory layout and the expected gain on our benchmark. Prefer a design that makes the work unneeded over one that makes the work cheaper.
4. Write a design document per subsystem in `docs/`. The owner reviews it before any code is built.
5. Build the chosen design. Run the same benchmark again on the whole browser, not only single rows. Record the numbers in `docs/progress.md`.

A limit or a budget that protects against a slow algorithm is a sign of a design problem. Fix the algorithm, then remove or lower the limit.

## Benchmark rules

- Measure a frozen commit. Main stays open while we measure.
- Rebase a change on the newest main and measure it again there before merge.
- Trust instruction counts under load. Trust wall time only on an idle machine.
- Run each case at least 3 times. Give the machine, the inputs, the run count and the spread.
- Mark an unverified cause as "hypothesis".

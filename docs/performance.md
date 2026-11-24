# Performance

Performance and resource use come first in `bare`: CPU instructions, peak memory and time to ready. Every change is weighed against them.

## How bare is measured

- **Instruction counts.** CPU cost is the number of user-mode instructions of one command (`perf stat -e instructions:u`), the minimum of 3 runs. Instruction counts hold under machine load. Wall time is trusted only on an idle machine. `tools/js/instr.sh` does this for one command.
- **Peak RSS.** Peak resident memory comes from a `wait4` wrapper, the median of 3 runs, each on a fresh copy of the binary that does not share disk blocks with the original (`cp --reflink=never`).
- **Cost rows.** A matrix of 800+ rows covers the engine, the parser, style, layout, paint, network code and sessions (`tools/js/cost_matrix.sh`). A change runs the rows of the code it touches. The rule per row is at most +0.5% instructions, under both the glibc and the jemalloc allocator, or within +0.5% with one codegen unit.
- **Journeys on recorded tapes.** `bare-drive` runs user journeys through the real input path. The journeys cover 48 public sites and a set of load ramps that grow a page until it is large. The network layer records replies to a tape and replays them with a virtual clock, so a journey does the same work in each run (see [networking.md](networking.md)).
- **Conformance gates.** The test262 counts of each directory must not drop, and the engine and the browser tests run before a merge.
- **Load tests.** `bare-stress` runs load cases in separate processes. `bare-bench` times and measures each module.

## Current numbers

Machine: AMD Ryzen 9 5900HX, 8 cores, 16 threads, Linux. Each row is the minimum of 3 runs for instructions and the median of 3 for RSS. Source: [final engine benchmark](internal/progress/2026-11-23-final-engine-benchmark.md), which also lists the inputs, the load of the machine and the spread.

### Vex alone: 16 programs

| Measure | Value |
| --- | ---: |
| Instructions, total over the 16 programs | 2.640 G |
| Instructions, geometric mean | 38.90 M |
| Peak RSS, sum | 106.73 MiB |
| Peak RSS, geometric mean | 6.57 MiB |
| Empty script, instructions | 0.70 M |
| Empty script, peak RSS | 5.95 MiB |

### Whole browser, on recorded tapes

| Workload | Rows | Instructions | Peak RSS (sum) |
| --- | ---: | ---: | ---: |
| Public journeys, clean rows (one large site left out) | 30 | 20.04 G | 1,023.97 MiB |
| Load ramps | 26 | 31.77 G | 957.82 MiB |
| React typing page | 1 | 5.41 G | 29.94 MiB |

These are the clean rows of the report. It defines a clean row and lists the rows left out and why.

### Conformance

Vex passes 29,217 test262 tests (see [vex.md](vex.md)).

## Reading the numbers

An instruction count is a cost, not a speed. The ranges in the report show how much a row moves between runs. A cause that no run proved is marked "hypothesis" in the working notes under [internal](internal/README.md).

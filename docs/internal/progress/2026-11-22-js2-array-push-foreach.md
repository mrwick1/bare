# Step through dense arrays in js2 forEach

The prelude lowers its private forEach step to an indexed read and the existing interpreter call. A present own element supplies the callback arguments directly. The fallback checks presence before it reads the property. The next iteration reads the current receiver state. The defaulted `thisArg` parameter avoids an arguments object and keeps the public function length at one.

## Measurements

Main at `74495429` contains the direct append and key reuse. The comparison uses that tree on both sides. See the [direct append measurements](2026-11-22-js2-array-push-direct.md) for the machine, inputs, and measurement method. The instruction ranges are three hardware-counter runs with default glibc. The RSS measurement uses a static `wait4` parent and fresh binary copies. Each run checks the completion value.

| Input | Before, M instructions | After, M instructions | QuickJS direct, M instructions | Median peak RSS before → after, MiB (range) |
|---|---:|---:|---:|---:|
| `objects_1` | 63.157–63.170 | 61.157–61.170 | 53.475 | 6.629 (6.590–6.711) → 6.566 (6.535–6.734) |
| `objects_10` | 589.939–590.284 | 570.132–570.468 | 512.457 | 10.848 (10.754–10.867) → 10.559 (10.547–10.820) |

The Callgrind forEach phase is the full input minus the push prefix. It fell from 127.611 M to 109.791 M instructions, a reduction of 17.820 M. The full script returned `198902497,1199980`.

In a 31-pair RSS sensitivity run at scale 10, both binaries used the same executable path. Median RSS was 10.781 → 10.801 MiB. The ranges were 10.422–10.863 and 10.594–10.906 MiB. The samples show a clear RSS reduction across Fix 2 as a whole. The forEach change by itself has a small RSS difference within the observed spread.

| Control | Before, M instructions | After, M instructions |
|---|---:|---:|
| `empty_1` | 0.7199–0.7205 | 0.7170–0.7186 |
| `calls_10` | 688.8264–688.8269 | 682.8232–682.8242 |
| `closures_10` | 119.7759–119.7766 | 120.1234–120.1242 |
| `throws_10` | 286.4529–286.4533 | 282.3180–282.3188 |

| Cost row | glibc change | jemalloc change |
|---|---:|---:|
| `interp_load loopfn 3000000` | -0.6926% | -0.6900% |
| `interp_load calls 1000000` | -1.2252% | -1.2180% |
| `interp_load fib 27` | +0.8949% | +0.8826% |
| `interp_load closures 1000000` | +0.4119% | +0.4111% |
| `interp_load objects 1000000` | -1.7469% | -1.7388% |
| `js2_gen_load next 100000` | -0.646% | -0.622% |
| `js2_lex_load small 1 1` | +0.001% | -0.016% |
| `js2_parse_load full stmts 1` | -0.056% | +0.035% |
| `js2_promise_load then 100000` | -0.326% | -0.313% |

## Own-slot trials

The design requires a gain on both fixed-key controls. Both trials reused the matched shape for slot geometry. The second also skipped repeat lazy preparation on an own read. Both sped up writes and slowed down reads, so this PR keeps the existing slot path. Values are the minimum of three runs before the compiler metadata merge.

| Control | Existing, M instructions | Reuse shape, M instructions | Reuse shape and skip repeat preparation, M instructions |
|---|---:|---:|---:|
| Fixed-key dictionary write | 184.507 | 182.745 | 181.724 |
| Fixed-key dictionary read | 198.533 | 201.171 | 205.548 |
| Fixed-key tree read | 191.448 | 192.848 | 198.247 |

## Tests

The host presence-order test failed before the change and passes with the new step. The topic tests cover holes, present undefined values, inherited getters, read-only descriptors, array-like receivers, callback mutations, length shrink, `thisArg`, throws, forced collection, fuel, and verifier window overlap. The narrow compiler and verifier tests and the regenerated prelude facts check passed. `cargo check --locked -p bare-js2 -p bare` passed.

The focused test262 comparison retained 1,354 passes against `74495429`. A comparison against `fc2fe21e` before direct push and key reuse gave the same counts.

| test262 directory | Pass / fail / skip before and after | Lost passes |
|---|---:|---:|
| `built-ins/Array/prototype/forEach` | 185 / 1 / 4 | 0 |
| `built-ins/Array/length` | 29 / 0 / 1 | 0 |
| `built-ins/Object/defineProperty` | 1,122 / 6 / 3 | 0 |
| `language/expressions/property-accessors` | 18 / 3 / 0 | 0 |

## Cost debt

`interp_load fib 27` exceeds +0.5% under both allocators. With both trees built using `-C codegen-units=1`, it changes by +0.8948% on glibc (248,628,549 → 250,853,164) and +0.8827% on jemalloc (252,066,226 → 254,291,138). The handwritten fib bytecode never runs the new step. Hypothesis: the hot dispatch code layout changes the cost of its existing operations. The other eight rows pass in the default build.

Not tested locally: full test262, browser journeys, Windows, and Miri.

# Reuse short computed keys in js2

The compiler selects `AddKey` when an addition feeds a computed member read or assignment. Short Latin-1 strings and tagged integers produce bytes in a stack buffer. The atom table reuses the existing key on a hit. Other operands use ordinary addition. The property operation still checks the receiver and converts the result to the correct string, index or Symbol key.

## Measurements

The baseline includes the direct append from #915 at `2f79e987`. See the [direct append measurements](2026-11-22-js2-array-push-direct.md) for the machine, build, inputs and method. Instruction ranges compare three default-allocator hardware-counter runs. RSS ranges come from three fresh copies with a static `wait4` parent.

| Input | Before, M instructions | After, M instructions | QuickJS direct, M instructions | Median peak RSS before → after, MiB (range) |
|---|---:|---:|---:|---:|
| `objects_1` | 68.029–68.033 | 63.275–63.284 | 53.475 | 7.137 (7.070–7.191) → 6.703 (6.648–6.719) |
| `objects_10` | 638.303–638.338 | 590.218–590.224 | 512.457 | 16.945 (16.703–16.977) → 10.715 (10.711–10.852) |

The write prefix fell from 345.979 M to 297.774 M Callgrind instructions. The `objects_10` completion value stayed `198902497,1199980`.

| Control | Before, M instructions | After, M instructions |
|---|---:|---:|
| `empty_1` | 0.7255–0.7263 | 0.7254–0.7261 |
| `calls_10` | 688.8503–688.8512 | 688.8504–688.8512 |
| `closures_10` | 119.8245–119.8255 | 119.8248–119.8254 |
| `throws_10` | 299.1251–299.1264 | 286.4953–286.4957 |
| `concat_1` | 0.8660–0.8662 | 0.8656–0.8662 |
| `concat_10` | 1.3653–1.3656 | 1.3652–1.3655 |

The hot dispatch experiment reached 564.901 M instructions on `objects_10`. It raised the calls, fib and closures micro rows by 0.52–0.77% under both allocators. The final opcode runs in cold dispatch. All nine cost rows are within +0.5% on both allocators.

| Cost row | glibc change | jemalloc change |
|---|---:|---:|
| `interp_load loopfn 3000000` | -0.0001% | -0.0000% |
| `interp_load calls 1000000` | -0.0001% | +0.0001% |
| `interp_load fib 27` | -0.0003% | +0.0005% |
| `interp_load closures 1000000` | +0.0001% | -0.0002% |
| `interp_load objects 1000000` | +0.0002% | -0.0002% |
| `js2_gen_load next 100000` | +0.118% | +0.114% |
| `js2_lex_load small 1 1` | -0.000% | -0.000% |
| `js2_parse_load full stmts 1` | -0.000% | -0.022% |
| `js2_promise_load then 100000` | +0.000% | +0.000% |

## Tests

The opcode selection and heap growth tests failed on the baseline. After the change, 8,192 repeated key writes grew no string blocks. Narrow key, integer concatenation, compiler and verifier tests passed. The key cases cover receiver and RHS order, coercion, numeric and named keys, both operand orders, wide and long strings, Symbols, and forced collection. `cargo check --locked -p bare-js2 -p bare` passed.

| test262 directory | Pass / fail / skip before and after | Lost passes |
|---|---:|---:|
| `language/expressions/addition` | 47 / 1 / 0 | 0 |
| `language/expressions/assignment` | 101 / 8 / 0 | 0 |
| `language/expressions/compound-assignment` | 391 / 64 / 0 | 0 |
| `language/expressions/logical-assignment` | 81 / 0 / 0 | 0 |
| `language/expressions/property-accessors` | 18 / 3 / 0 | 0 |
| `built-ins/Array/length` | 29 / 0 / 1 | 0 |

Not tested locally: full test262, browser journeys, Windows, and Miri.

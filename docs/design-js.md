# Design: JS execution and parsing

Status: proposal. The owner reviews it before any code is built.
Frozen commit: `f6415364`. Machine: archMachine. Date: 2026-11-13.
Scope: `crates/js` (parse, evaluate, heap, strings, regex, GC). Out of scope: CSS and style (see `docs/design-style.md`), layout, paint.
Web access worked. Every outside claim has a URL. A claim I could not read is marked "to verify".

Label rules. "Measured" means I ran it or read it in a report. "Estimate" means a number I computed from measured numbers, and the calculation is shown. "Hypothesis" means a cause I have not proved.

## 1. Summary

Four facts decide the design.

1. **Most JS that we parse never runs.** On 45 public journeys, 94.9% of syntax nodes sit in functions that were never called (measured, section 2.3). We parse them, walk them once, compile their regex literals, and keep their trees in memory. This is 6.5% of CPU for parse, about 1.9% for the first walk, and 19% of the peak memory on the heaviest journey.
2. **The interpreter is a small part of the journey cost.** Dispatch and calls are about 3.4% of journey instructions. They are most of the cost on the load ramps (JS is 57.7% of ramp instructions).
3. **The bytecode lane stopped for a design reason, not a tuning reason.** It kept the walker's environments and its Rust recursion. A byte op costs 46 instructions. A mature register VM spends far fewer. Section 4 explains it.
4. **Two work-multipliers are in the strings and the regex compiler.** Every string is interned, so `s += x` is O(n) per step and O(n²) per loop (a 40,000-step loop fails at the 1 GiB heap limit, measured). A `/i` character class scans 65,536 positions (read from code).

Recommended design, in the order of the work removed:

| Phase | Idea | Work removed |
|---|---|---|
| P1 | Lazy functions: preparse bodies, parse and walk on first call, compile regex literals on first use, share compiled regexes | parse, first walk, regex compile, AST memory of never-called code |
| P2 | Strings: shared atoms, no interning of long strings, ropes for `+` | intern cost, O(n²) concat |
| P3 | Register VM stage 2: locals in registers, frames in one loop, fused ops, GC safepoints | Env and Rust-recursion cost of every call and loop |
| P4 | The AST becomes transient: compile at first call, drop the tree | AST memory of called code |
| P5 | Compact objects with inline slots | object bytes |
| P6 | Regex: measure first, then a linear fallback and a lower step limit | regex exec and the 50 M step limit |
| P7 | 8-byte `Value` (optional) | value bytes |

Estimated total on 48 journeys: about 7 to 9% fewer instructions (estimate). Estimated on JS ramps: about 45% fewer. Estimated on the peak memory of python-docs-search-results: about 28% less with P1 and P5, up to 38% with P7 and Latin-1 strings. A perfect JS engine would save at most 32% of journey instructions (25.5% execution plus 6.6% parse), so the journeys are not JS-bound. Phases P1 and P2 give most of the journey gain. Phase P3 gives most of the ramp gain.

## 2. What we measured

### 2.1 Facts given (from `/data/bare-tmp/prof/cpu/report.md`, `prof/mem/report.md`, `bm/a`, `bm/c`)

| Fact | Value |
|---|---|
| JS execution, 48 journeys | 25.5% attributed (52.3% inclusive); no dominant function |
| Top JS functions | regex `exec_in` 2.55%, `Heap::intern` 1.48%, regex `add_class` 1.10% |
| JS parse, 48 journeys | 6.6% (30.7% on nodejs-org-home, 29.4% on crates-io-home, 29.2% on react-docs) |
| JS execution, ramps | 57.7% |
| One budget step | about 19 ns; one simple loop iteration is about 9 steps |
| python-docs-search-results peak | 116 MiB: JS heap 39% (45.4 MiB), script ASTs 19% (22.2 MiB) |
| One JS object | 84 B empty, 136 B with 3 properties |

### 2.2 My measurements

Tools: `perf stat -e instructions:u`, frame-pointer samples of the frozen build, a private instrumented copy of the source. The copy and all scripts are in `/data/bare-tmp/design/js/` (not in the repository). Every run used `/data/bare-tmp/bin/heavy`. Instruction counts repeat to about 0.1%. I ran each micro-benchmark once.

**Where the 25.5% goes** (attributed shares, top 400 functions, 24.6% of 25.5%):

| Module | Share of journey instructions |
|---|---|
| `eval*` (walker, first walk, builtins glue) | 10.6% |
| `heap*` (intern, define, lookup, alloc, collect) | 9.3% |
| `regexp` (exec 2.2%, class compile 1.1%) | 3.4% |
| other | 1.2% |

**Inclusive shares** (220,573 samples, one per 400,000 instructions; `out/incl_js.json`):

| Tag | Samples | Share |
|---|---|---|
| parse (`js_parse`) | 14,377 | 6.5% |
| first walk (`walk`) | 4,292 | 1.9% |
| regex exec | 4,974 | 2.3% |
| regex compile (`rx_new`) | 2,688 | 1.2% |
| intern | 4,633 | 2.1% |
| GC | 3,358 | 1.5% |
| JSON | 3,615 | 1.6% |
| realm creation | 1,128 | 0.5% |

Interpreter dispatch is small. The interpreter functions (`eval_expr_inner` 0.79%, `machine` 0.51%, `get_name` 0.26%, `test_true` 0.23%, `eval_assign` 0.22%, the call helpers and others) add to 2.35%, and `link_inner` (member and call) is 1.00%. That is about 3.4%. `for_in_keys` alone is 0.98% (to verify why; hypothesis: it allocates a key list per loop).

**The lexer is about 42% of parse.** `Lexer::next` inclusive is 5.26 of 13.12 points on github-docs-home and 6.19 of 14.80 on nextjs-docs (perf, one run each).

**Micro-benchmarks** (`design_run`, `instructions:u`, baseline of an empty script, 2.25 M, removed; the iteration counts are in the JS files):

| Program | tree | byte | Note |
|---|---|---|---|
| empty `for` loop, locals, 200,000 iterations | 819 / iteration | 410 / iteration | byte: 9 ops, 46 instructions per op |
| the same loop with globals | 1,049 / iteration | 585 / iteration | |
| `s += o + o + o`, globals | +1,395 | +763 | 4 reads, 1 write, 3 adds |
| `s += o.x + o.y + o.z` instead | +552 more than the line above | +250 more | 3 inline-cache reads: 184 (tree), 83 (byte) each |
| `fib(23)`, 92,735 calls | 2,001 / call | 1,895 / call | |
| `[].push({x,y,z})` plus 3 reads, 100,000 elements | 11,036 / element | 9,518 / element | |
| `s += 'x'`, 5,000 / 10,000 / 20,000 steps | 5,370 / 8,190 / 13,830 per step | 4,880 / 7,710 / 13,330 | grows with n |
| `s += 'x'`, 40,000 steps | `Limit(Heap(Bytes))` | same | 1 GiB heap limit |
| `/^[a-z]+\d*$/i.test(s)` in a function | +4,240 / call | +4,270 | literal object plus match of 4 chars |
| `new RegExp(p, 'i').test(s)`, 5 distinct patterns | about +7,750 / call | | against the loop without a regex; includes the cache lookup |

`eval_load` rows at the frozen commit (`out/exec_ratio.tsv`): `loop` tree 5.015 G, byte 2.645 G (1.90x). `loopfn` 4.091 G and 1.881 G (2.18x). `calls` 1.28x. `fib` 1.06x. `closures` 1.18x. `objects` 1.00x. `webpack` 0.73x (byte is slower). Byte did not change since the R1 run (2.645 G both times). The tree walker got 24% faster (6.575 G then, 5.015 G now). So the R1 gate would now read 1.90x, not 2.49x.

### 2.3 How much JS never runs

I counted, per journey, the functions that `call_script` ran, and the syntax nodes inside the outermost functions that never ran. 45 of 48 journeys logged. Four journeys failed an `expect` step after about 4 s in my private build: crates-io-home (`expect css="body" visible`), mdn-reference (`title~"MDN"`), python-docs-search-results (`div.related:nth-of-type(2) input[name=q]`) and hn-new-past (`tr.athing` count). The first three wrote no log. I did not find the cause (hypothesis: the live sites changed since the frozen run; to verify).

| Total over 45 journeys | Value |
|---|---|
| scripts / functions / functions called | 549 / 24,580 / 788 (3.2%) |
| syntax nodes (expressions) | 974,890; 925,431 (94.9%) in never-called functions |
| AST bytes | 34.4 MB; 32.5 MB (94.6%) in never-called functions (estimate: bytes by node share) |
| per-script cache bytes (`Code`) | 10.8 MB; 10.2 MB (94.5%) in never-called functions (estimate) |
| source bytes | 11.9 MB; 5.3 MB (44%) in never-called functions |
| source bytes without rust-book-search (a 5 MB data script) | 6.9 MB; 5.0 MB (72%) |

Per journey, the never-called share of AST bytes is 87% (python-docs-search) to 100% (nodejs-org-home, substack-home). Big cases: github-docs-home 9.5 MB AST with 88% dead, nextjs-docs 7.7 MB with 99%, python-org 2.8 MB with 99%.

Caveats. The journeys are short, so a page that runs longer calls more. A function that only the byte lane calls is not counted (I ran the tree lane). I have no log for python-docs-search-results, so its dead share (22.2 MiB of AST) is a hypothesis: I assume 90%, the value of the similar pages.

### 2.4 Regex

Over 45 journeys (`out/agg.json`):

| Measure | Value |
|---|---|
| regex literals compiled at first walk | 2,106 |
| of them, first-evaluated at least once | 1,197 (57%) |
| compiled and never evaluated | 909 (43%) |
| unique patterns per journey, summed | 1,167: 939 compiles (45%) repeat a pattern already compiled in the same journey |
| `new RegExp` compiles (dynamic) | 94; 53 unique |
| compile wall time (literals) | 12.2 ms over 45 journeys; most: github-docs-home 4.05 ms, nextjs-docs 2.46 ms |
| compile wall time (dynamic) | 6.5 ms; 3.2 ms on each python-org journey |

The wall time is lower than the instruction share suggests (hypothesis: compile code runs at a high instructions-per-cycle rate; the rule in `CLAUDE.md` is to trust instruction counts). Each realm compiles its own copy: the compiled regex is not shared between realms.

### 2.5 Who calls `intern`

Of the samples in `Heap::intern` (2.04% of all, 48 journeys, one run each), the callers are:

| Caller | Share of intern |
|---|---|
| `Dom::js_str` from `browser_fetch::consume` (the body of a response becomes a JS string) | 14.0% |
| `key_string` from `prop_key` (property keys) | 12.8% |
| `host_function_with` (installing host functions) | 10.8% |
| `string_literal` from `eval_array` | 7.5% |
| JSON parser keys and strings | 7.1% |
| `binary` (concat) | 5.1% |
| `Dom::key` and `install` paths (style, computed, fetch) | about 7.7% |
| `text_content` | 2.8% |

About 18% is host and builtin names that every new realm interns again. About 21% is long content strings (fetch bodies, JSON, text) that nothing compares by identity.

## 3. The current design

Complexities: n is the string length, p is the property count, s is the step count.

| Part | Design (file:line) | Time | Memory |
|---|---|---|---|
| AST | One arena `Vec` per node kind in `Program` (`ast.rs:575`). `Expr` is 24 B (`ast.rs:185`), `Stmt` 32 B, `Pat` 20 B, `Func` 44 B, `Prop` 28 B (measured with `size_of`). A node refers to another by index (`ExprId`) | build: O(source) | 24 B per expression plus the cache below |
| Script cache | `Code` per script (`eval_scripts.rs:46`): `expr_name` 4 B per expression, `pat_name`, `stmt_label`, `scopes` 32 B per function, `site_strs`, `prop_str` | built at first walk, O(nodes) | 31% of the AST bytes (10.75 MB over 34.4 MB) |
| Parser | Recursive descent plus Pratt, early errors checked while parsing (`parse.rs`, 3,453 lines; `lex.rs`, 1,950 lines). It resolves names into scopes while it parses | O(source) | |
| Parse cache | Thread-local, shared by realms, `MAX_BYTES` 32 MiB, `MIN_SOURCE` 512, `MAX_IDLE` 2 realms (`parse_cache.rs:26-30`) | hit: O(1) | up to 32 MiB |
| First walk | `w_expr` (`eval.rs:1563`) visits every node of a script once per realm: it links names, sets up scopes, compiles regex literals (`ext_walk_regexp`, `eval_builtins.rs:397`) | O(nodes) | |
| Evaluate | `eval_expr_inner` (`eval.rs:2942`) matches `ExprKind` and recurses in Rust. `tick` (`eval.rs:1268`) adds 1 to `steps` per statement and expression and compares it to the limit | O(s) | Rust stack per nesting level |
| Name read | `get_name` (`eval.rs:2659`) then `fast_binding` (`eval_scope.rs:312`): a packed `u32` per site gives a frame slot (`fp + index`) or a hop count over `Env` parents, then checks `binding.name == n` and the `INIT` flag. A global goes to `get_global` (`eval.rs:2680`): a position hint on the global object | O(hops) | `Binding` 24 B, `Env` 56 B |
| Closure variable | Same path with hops. A function whose variables are captured keeps an `Env` (`eval.rs:407`); others use frame slots when the gate `fr_gate` allows | O(hops) | |
| Property read | `link_inner` `Member` (`eval.rs:2839`): `ic_own` (`heap_shape.rs:926`) compares the receiver shape with the site's `Ic` (24 B, `heap_shape.rs:748`). A miss goes to `member_ic` (`eval_ic.rs:107`) | hit: O(1), 184 instr (tree) | |
| Call | `Call` in `link_inner` (`eval.rs:2866`): `eval_args` (pooled `Vec`), `call_fn`, `call_script` (`eval.rs:2224`): `select` script, then `call_frame` or `new_env` (`eval.rs:1820`) plus `bind_call` (`eval.rs:2273`) plus `in_frame`, all by Rust recursion | O(args) plus about 2,000 instr | one `Env` or frame per call |
| String concat | `binary` calls `Heap::concat` (`heap.rs:1010`): copy both strings into a new buffer, hash all of it, probe the interner (`heap.rs:955`), allocate a `Str` | O(n) per step, O(n²) per loop | O(n) per step, kept until the next collect |
| `Value` | 9-variant `Copy` enum, 16 B (`value.rs:46`). Handles are `{index: u32, gn: u32}`, 8 B | | |
| Object | `Object` 80 B in `Slab<Object>` (`heap.rs:382`, `heap.rs:473`) plus 8 B `Meta`, 4 B `back`, 1 B mark. Named properties: a shape id and `slots: Vec<Value>` (16 B per property), or in dictionary mode a `PropMap` (`heap.rs:309`): 48 B per entry, a hash index above 8 entries | shape hit: O(1) | 136 B for 3 properties |
| String | `Str` 24 B plus 2 B per UTF-16 unit in a `Box<[u16]>`; every string is interned in an open-addressing table at load ≤ 0.5 (`heap.rs:612`, `heap.rs:955`). Equal strings have equal `StrId`, so `===` is one compare | intern: O(n) hash, O(1) probe | about 100 B for a 7-char string |
| GC | Stop-the-world mark and sweep (`heap.rs:2234`). The sweep compacts `live` with the `Meta.dense` indirection. A collect runs only when `depth == 0 && stmt_frames == 0` and the heap is above `max(8 MiB, 1.75 x live after the last collect)` (`eval.rs:1104-1108`, `eval.rs:223`) | O(live) | |
| Regex | Backtracking VM over `&[u16]` with an explicit stack and an ASCII first-set accelerator (`regexp.rs:315` `exec_in`). One step limit of 50 M (`regexp.rs:154`). Compile: parser, `add_class` (`regexp.rs:1761`), instruction list (`Regex::new`, `regexp.rs:215`) | exec: worst case exponential, cut by the limit | |
| Regex cache | Literals: per realm, compiled at first walk, a new object per evaluation (`ext_eval_regexp`, `eval_builtins.rs:412`). `new RegExp`: cache by (pattern, flags), cleared at every collect and at 256 entries (`eval_builtins.rs:1245`, `ext_prune`) | | |
| Step budget | One counter, `max_steps` 100 M by default (`eval.rs:132`). The tree lane counts a step per node. The byte lane counts one per back edge and call (measured: `steps=200000` for 200,000 iterations) | | |
| Exec mode | `Exec::{Tree, Auto, Byte}`, default `Tree`, `BARE_JS_EXEC` (`eval.rs:132`). `bc_enter` (`bc_vm.rs:179`), `bc_script` (`bc_vm.rs:206`), `bc_loop` (`bc_vm.rs:572`) | | |

The byte lane (a register VM, 32-bit words `[op:8][a:8][b:8][c:8]`) exists behind `BARE_JS_EXEC`. Format, generator and verifier are in `bc*.rs`.

## 4. Why the bytecode lane fell short, and what would be different

Plain words: a bytecode VM is only fast if its instructions are small. In JS terms, the byte lane compiled `for` loops to a flat array, but each instruction still did `envs[e].bindings[i]` and checked a name, like reading `scope[name]` from a `Map` where a mature VM reads `regs[3]` from an array.

Cause, from the code and the measurements:

1. **Locals stay in `Env` bindings.** `GetEnv` and `SetEnv` go through `bc_slot` (`bc_vm.rs:296`), `envs[e]`, `bindings.get(i)`, a name check and a flags check. This is the walker's name read with a new loop around it. The plan called the fix "stage 2" (B12, registers for uncaptured locals). It was never built.
2. **No fusing.** `for (var i = 0; i < n; i++) s += i` is 9 ops per empty iteration (`tools/fixtures/js/bc-golden.txt:658-680`): the compare, the jump, and `i++` as four ops (`GetEnv`, `ToNumeric`, `Inc`, `SetEnv`). A fused compare-and-jump and an in-place `Inc` give 2 to 3.
3. **Calls leave the loop.** A JS-to-JS call goes through `call_script`, `bc_enter`, `new_env`, `bind_call` and `in_frame` by Rust recursion. `fib` costs 1,895 instructions per call in byte and 2,001 in tree. That is why calls gain 1.06 to 1.28x. The plan's B4 (frames in the loop) was never built.
4. **Globals cost more than locals.** The R1 gate ran on `loop`, with globals. A cell access cost about 55 instructions more than `GetEnv` (`docs/progress.md`, "B3: the bytecode VM loop stops at the R1 gate").

Measured per op: 46 instructions on locals (410 per 9 ops) and 73 on globals (progress.md: 18 ops, about 1,320 per iteration). I do not have a measured number for a mature interpreter. JSC reports 3.97 ns per bytecode in its LLInt and 1.71 ns in its Baseline JIT, from a regression on its own benchmarks (https://webkit.org/blog/10308/speculation-in-javascriptcore/). That is an order of magnitude, not an instruction count.

What is different in this design:

| | Byte lane (stage 1) | This design (P3) |
|---|---|---|
| Locals | `Env` bindings, name check | registers in one `Vec<Value>` stack, plain index |
| Captured locals | `Env` | a boxed cell, only for captured variables (as Lua upvalues) |
| Call | Rust recursion, `new_env`, `bind_call` | push a frame on the same stack, no Rust recursion |
| Loop test | compare plus jump | one fused op |
| `i++` | 4 ops | 1 op |
| Budget | one per back edge and call | the same, as an interrupt budget |
| GC roots | Rust locals (so only at depth 0) | the register stack: precise, so GC can run at a back edge |
| Gate | 3x on `loop 10` | a spike first: loopfn at least 4x, `fib` at least 3x against tree, or stop |

Boa has bytecode and lost to bare-js on 6 of 8 rows (`docs/engine-plan.md`, R1). That matches the finding: bytecode alone is not faster.

## 5. What real engines do

| Engine | Claim (from the source) | URL | Use here |
|---|---|---|---|
| V8 preparser | The preparser skips function bodies, builds no AST, checks syntax and tracks variable allocation and early errors | https://v8.dev/blog/preparser | P1 |
| V8 Ignition | Register machine with an accumulator; bytecode is 25 to 50% the size of baseline machine code | https://v8.dev/blog/ignition-interpreter | P3, P4 |
| V8 Sparkplug | One linear pass from bytecode, no IR; Speedometer +5 to 10%; same frame layout as the interpreter | https://v8.dev/blog/sparkplug | rejected (a JIT) |
| V8 pointer compression | Heap down to 43%, renderer memory down to 20%; 4 GB cage, 31-bit Smi | https://v8.dev/blog/pointer-compression | P7 |
| V8 Irregexp | Bytecode interpreter first, native code when hot; heap code size down 4 to 7% | https://v8.dev/blog/regexp-tier-up | P6 |
| V8 linear regex | A backtracking engine falls back to a linear NFA after 50,000 backtracks; no backreferences or lookarounds in the linear engine | https://v8.dev/blog/non-backtracking-regexp | P6 |
| V8 hidden classes | Objects with the same properties share one HiddenClass; in-object properties need no indirection; dictionary mode has no inline caches | https://v8.dev/blog/fast-properties | P5 (we have shapes already) |
| SpiderMonkey baseline interpreter | An interpreter with inline caches and the Baseline JIT frame layout; Google Docs load 901 ms (C++ interpreter) to 676 ms | https://hacks.mozilla.org/2019/08/the-baseline-interpreter-a-faster-js-interpreter-in-firefox-70/ | P3 (ICs matter as much as the loop) |
| SpiderMonkey CacheIR and Warp | ICs are a short linear bytecode; JS memory 8% lower, about 20% faster Google Docs load | https://hacks.mozilla.org/2020/11/warp-improved-js-performance-in-firefox-83/ | idea for later |
| JSC LLInt | An interpreter in portable assembly that shares the JIT ABI; tier costs 3.97 / 1.71 / 0.349 / 0.225 ns per bytecode | https://webkit.org/blog/10308/speculation-in-javascriptcore/ | cost scale |
| QuickJS | Stack bytecode, no parse tree, atoms, shapes, 8-bit or 16-bit strings; on 64-bit builds a value is 128 bits with no NaN boxing; a regex engine of about 15 KiB that backtracks with an explicit stack | https://bellard.org/quickjs/quickjs.html | P2, P3; shows 16 B values are workable |
| Hermes | Register bytecode, 1-byte register index, unique strings in one table, deserialization without copying (the page does not say mmap, lazy compile or no-JIT: to verify) | https://github.com/facebook/hermes/blob/main/doc/Design.md | P4 |
| Boa | Register and stack hybrid, a `Vec<JsValue>` shared by all frames, shapes and inline caches | https://raw.githubusercontent.com/boa-dev/boa/main/docs/vm.md | P3 |
| Nova (Rust) | Data-oriented arenas, hot data apart from cold, 32-bit handles, arenas compacted by GC, no NaN boxing | https://github.com/trynova/nova | P5 |
| Lua 5.0 | Register VM; locals live in the frame's registers; 32-bit instructions; fewer instructions than a stack VM; upvalues migrate into the closure when the frame ends | https://www.lua.org/doc/jucs05.pdf | P3 (read with `pdftotext`) |
| LuaJIT | "High-speed interpreter, written in assembler"; low memory footprint (internals not covered by the page: to verify) | https://luajit.org/luajit.html | cost scale |
| RE2 | Linear time and fixed stack; a DFA built on demand; the cache is freed and restarted when full; a bit-state backtracker when the visited bitmap is at most 32 KB | https://swtch.com/~rsc/regexp/regexp3.html | P6 |
| Rust `regex` | Worst case O(m·n); lazy DFA, one-pass DFA, bounded backtracker, PikeVM; "an anti-pattern to compile the same pattern in a loop" | https://docs.rs/regex/latest/regex/#performance | P1, P6 |

Not read: V8 ropes and `ConsString` (I know the name; I did not read a source: to verify), the V8 Irregexp blog (404), the SpiderMonkey GC blog (404), JSC LLInt internals past the first half of the page, Hermes lazy compile.

## 6. Sub-problems and options

Each option lists: data structures, algorithm, complexity, memory in bytes, work removed, risks, and the estimate with its arithmetic. "Current" is option A.

### 6.1 Execution model

| | A. Tree walker (current) | B. Register VM, stage 2 | C. Stack VM (QuickJS) | D. Closure compilation | E. Baseline JIT |
|---|---|---|---|---|---|
| Data | AST plus `Env` and frame slots | 32-bit words, registers in one stack, frames in one loop, boxed cells for captured variables | 1 to 3 byte ops, operand stack | one Rust closure per AST node | machine code per function |
| Algorithm | recursive `match` | `loop { match op }`, calls push a frame | same, more dispatches | indirect call per node | linear pass over bytecode |
| Per empty loop iteration | 819 instr (6 steps) | 3 ops x about 15 = about 45 (estimate) | about 6 ops (get, push, lt, jump, inc, jump) x about 15 = about 90 (estimate) | not estimated | not estimated |
| Per `fib` call | 2,001 | about 350 to 450 (estimate) | similar to B | not estimated | |
| Memory per function | AST 24 B per expression plus cache 11 B | about 4 to 12 B per expression (hypothesis) | 1 to 3 B per op (QuickJS claims compact code) | 16 to 48 B per node (hypothesis) | large |
| Risk | none | the R1 failure again (mitigated by a spike and a gate); `with` and direct `eval` stay in tree | needs a new generator | memory heavy | platform specific; against the cross-platform rule |

Estimate for B, with the arithmetic. Loop: `LtJmp r_i, r_n` fuses the test, `Inc r_i` is one op, and `Jmp` closes the loop. That is 3 ops. At 15 instructions per op (an assumption, to verify in the spike) the iteration costs 45. Now: 410 (byte), 819 (tree). Ratio: 9x against byte, 18x against tree. Call: `fib` body is `LtJmp`, `Sub`, `GetGlobal`, `Call`, `Sub`, `GetGlobal`, `Call`, `Add`, `Ret`: 9 ops x 15 = 135. A frame push and pop of about 100 instructions each, twice per call: 200. Total about 335, rounded to 350 to 450. Ratio: 4.4x to 5.7x against 2,001.

Why this option and not C. B stage 1 exists (`bc*.rs`: format, generator, verifier, golden listing) and stage 2 reuses all of it. C would restart the generator. QuickJS shows that a 16-byte value and a stack VM are fast enough for its goals, but fewer ops per statement is the reason a register VM wins in Lua's paper (https://www.lua.org/doc/jucs05.pdf).

Why not E. A JIT needs executable memory, a code-cache policy and per-OS code. `docs/engine-plan.md` and the memory note `bare-cross-platform` keep the core OS-independent. V8's Sparkplug gives +5 to 10% on Speedometer over an interpreter that is already fast; our interpreter is not.

Why not D. It keeps a tree in memory (more bytes than bytecode) and keeps Rust recursion. Memory is a first goal.

Effect on journeys (estimate): dispatch and calls are 3.4% of journey instructions. At 4.5x, 3.4% x (1 - 1/4.5) = 2.6% saved. Effect on ramps: JS is 57.7% of ramp instructions. At 4.5x, 57.7% x (1 - 1/4.5) = 45% of ramp instructions saved. On `steps_20000000` (100% JS loop) the saving is 78% (that ramp is one loop). 4.5x is the middle of my 4.4x to 5.7x call estimate and a lower value than the loop estimate.

### 6.2 Value representation

| | A. 16 B enum (current) | B. 8 B NaN-boxed | C. 16 B enum plus an `Int(i32)` variant |
|---|---|---|---|
| Layout | tag 8 B + payload 8 B; handle `{index u32, gn u32}` | an `f64`; other types in the NaN payload: 3 bits of type, 32 bits of index | as A |
| Algorithm | `match` | mask and compare; a number read needs no decode | integer fast path in `+`, `<`, `[]` |
| Memory | 16 B per slot, binding, element | 8 B | 16 B |
| Work removed | none | half of the value arrays | `f64` conversions for loop counters |
| Risk | | needs the handle without `gn` (the generation protects against a stale handle); about 100s of `match Value` sites; an `unsafe`-free encoding is possible with `u64` and helper methods | small |
| Estimate | | slots 9.4 MiB, so 4.7 MiB saved, plus array elements and bindings (not measured); about 4% of the 116 MiB peak (estimate) | under 1% of instructions (hypothesis) |

V8 pointer compression cut the heap by up to 43% (https://v8.dev/blog/pointer-compression), but V8's values are 8 B and become 4 B. Ours go 16 B to 8 B on the value arrays only. QuickJS runs with 16 B values and is small and fast (https://bellard.org/quickjs/quickjs.html). `docs/engine-plan.md` already lists "8-byte `Value`" as deferred. Recommendation: P7, optional, after P5. Decide with a measurement of how many of the 45 MiB JS heap bytes are value arrays.

### 6.3 Object layout

Measured on python-docs-search-results (heap profile, `prof/mem/report.md`): object slab 20.0 MiB (80 B each, about 262,000 objects), strings 11.3 MiB (`intern`), slot vectors 9.4 MiB (`add_data_fast`), dictionary entries 6.9 MiB (`PropMap::set`, 48 B each). The search index is parsed from JSON.

| | A. Current | B. Compact object with inline slots | C. Hot and cold split (Nova) |
|---|---|---|---|
| Data | `Object` 80 B in a slab plus a `Vec<Value>` for slots (an own allocation with 16 B malloc overhead) | a 32 to 40 B record: shape id, flags, elements index, and 3 inline value slots in a size-class slab (0 to 4 slots; more goes to an overflow `Vec`). The prototype moves into the shape | rare fields (exotic payload, lazy flags, dense element map) in side tables keyed by object index |
| Algorithm | shape lookup, `slots[i]` | shape lookup, `inline[i]` (no pointer chase) | as A, smaller record |
| Bytes, 3 properties | 136 | 40 + 3 x 16 = 88 (with 8 B values: 64) | 40 + a `Vec` of 48 |
| Work removed | | one allocation per object; one cache miss per read | |
| Risk | | the `Object` type is used all over `heap*.rs`; size classes add code | side-table lookups |
| Estimate | | slab 20.0 to 10.0 MiB (80 to 40 B per object); slot vectors 9.4 MiB lose the 16 B malloc header (about 2 MiB, hypothesis); so about 12 MiB, 10% of the 116 MiB peak | |

Dictionary mode costs 48 B per entry against 16 B per slot (3x). JSON objects whose keys repeat should take shapes. Whether the 6.9 MiB of `PropMap` entries belong to JSON objects: to verify. If they do, shapes plus inline slots cut about 4.6 MiB (6.9 MiB x (1 - 16/48), estimate).

Why this option. Shapes and inline caches exist (the CSS-and-heap work of lane A). This step removes the allocation and the indirection that remain. It is independent of P1 to P4. It does not move journey instructions much (`alloc_object` 0.48%, `ordinary_define` 1.03%, `define_own_property` 0.80%, `lookup` 0.72%: 3.0%; a 20 to 30% cut is 0.6 to 0.9%, hypothesis).

### 6.4 Strings and interning

Current rule: every string is interned, so equality is an id compare. The cost is paid at creation: hash O(n), probe, copy. Section 2.5 shows who pays.

| | A. Intern all (current) | B. Atoms plus long strings plus ropes | C. Latin-1 or UTF-16 storage |
|---|---|---|---|
| Data | `Str` {digit, `Box<[u16]>`, hash} 24 B; one table at load 0.5 | short strings (up to 32 units, hypothesis for the cut-off; to tune) interned as atoms; longer strings not interned, with a lazy hash; a rope node `Cat{left, right, len}` (24 B) for `+` results above the cut-off; a shared atom table built once per process for builtin and host names | `Str` holds `Box<[u8]>` when every unit is below 256 |
| Algorithm | `concat`: copy, hash, probe | `+`: O(1) rope node; flatten on first read of a unit, on use as a key or on regex exec: O(n) once; `===`: equal ids true; both atoms false; else compare length, hash, units | hash and compare bytes |
| Time | O(n) per `+`, O(n²) per loop | O(1) per `+`, O(n) per flatten | halves hash and copy for ASCII |
| Memory per `+` | O(n) | 24 B | 50% of payload for ASCII |
| Work removed | | interning of long content strings (about 21% of intern); re-interning of host names per realm (about 18%); O(n²) | half of the string bytes |
| Risk | | `strict_equals` (`value.rs`) and every `Map`, `Set` key and `switch` use `StrId` equality: they need the two-class rule; property keys must be atoms | `str_units` returns `&[u16]` and has many callers: it becomes a view type |
| Estimate | | intern 2.1% x (21% + 18%) = 0.8% of journey instructions; the O(n²) loop falls from 13,830 per step at 20,000 to about 300 (estimate) | strings 11.3 + 8.1 = 19.4 MiB on the heaviest journey; 8-bit saves about 40% of it (payload only; the header stays): about 8 MiB (hypothesis: the JSON strings are ASCII) |

JS analogy for B: `s += x` in a loop is like `parts.push(x)` and one `parts.join('')` when you read it. A rope is that, done by the engine.

Recommendation: B in P2. C only if memory is still above the goal after P5, because it is a wide API change.

### 6.5 Regex

Measured: exec 2.2 to 2.55%, compile 1.1 to 1.2%. Compile has three causes visible in the data and code:

1. 43% of the compiled literals never run (section 2.4).
2. 45% of compiles repeat a pattern already compiled in the same journey.
3. The `/i` class path (non-Unicode `i`) builds a `vec![0u64; 1024]`, runs `canon` on every member, then scans all 65,536 positions to rebuild ranges (`regexp.rs:1783-1800`). Hypothesis: this is the `add_class` 1.1%. The Unicode `i` branch above it (`regexp.rs:1763-1782`) works per range and shows the cheaper design.

| | A. Current | B. Lazy plus shared compile plus range fold | C. Linear fallback engine | D. Lazy DFA for `test` and `search` |
|---|---|---|---|---|
| Data | `Regex` program, per realm | a process-wide cache keyed by (pattern units, flags), bytes-capped LRU; a literal compiles at first evaluation; class fold works on ranges | a PikeVM (Thompson NFA with thread list) used when backtracks pass a threshold and the pattern has no backreference or lookaround | states built on demand, cache freed when full (RE2) |
| Time | compile O(p + 65,536 per `/i` class) | compile once per pattern, class fold O(ranges) | O(m·n) worst case | O(n) after warm-up |
| Memory | per realm | one copy; the program of a never-run literal is never built | 2 thread lists of m entries | a capped state cache |
| Work removed | | 43% (never run) then 45% (repeats) of compile | exponential cases; the 50 M step limit | per-character cost of `test` |
| Risk | | an invalid pattern must still fail at load: a validate-only pass is needed (cost to measure) | JS semantics (captures in loops, lookbehind) limit the engine; V8 supports no backreference or lookaround in it | the same limits; a lot of code |
| Estimate | | compile 1.2% x (1 - 0.57 x 0.55) = 0.8% saved of journey instructions (estimate: lazy keeps 57%, sharing keeps 55% of those; the two effects are not independent) | no journey gain (hypothesis: no journey hits the limit; not measured) | `exec_in` 2.2 to 2.55%: unknown until profiled per pattern |

I did not log regex exec by pattern, input length and steps, so I cannot say which design fits the 2.55%. Hypothesis: it comes from unanchored scans of long strings (page text), not from per-step cost. P6 starts with that measurement.

Recommendation: B inside P1 (it rides on lazy functions). C and D in P6, only with the exec profile. The step limit falls with C: the V8 default of 50,000 backtracks before the fallback is the reference (https://v8.dev/blog/non-backtracking-regexp).

### 6.6 Parsing and AST lifetime

| | A. Parse all, keep all (current) | B. Preparse, parse on first call, keep the tree | C. Preparse, compile on first call, drop the tree | D. A persistent code cache |
|---|---|---|---|---|
| Data | arenas for all nodes | a function stub (`Func` 44 B, no body) with the source span; the body parses at first call into the arenas | a stub; at first call, parse the body, compile to bytecode, drop the nodes | a disk cache keyed by source hash: function spans and the early-error verdict |
| Algorithm | one pass | pass 1: lex plus syntax and early-error check plus free-variable set, no nodes; pass 2 (only called functions): full parse | like B, then pass 3: bytecode | skip pass 1 on a hit |
| Time | 1.0 | see below | see below | hit: near 0 |
| Memory | 24 B + 11 B per expression, all of it | the same for called code only (5.1% of nodes) plus about 80 B per function | bytecode for called code only | as B or C |
| Risk | | the free-variable set must be exact (a missed capture breaks a frame-slot or register decision); direct `eval` and `with` make a scope dynamic (V8 does the same, https://v8.dev/blog/preparser) | needs P3; `with` and direct `eval` fall back to a re-parsed tree | the cache is state on disk: it changes determinism and the hosting rule; not recommended now |

Estimate for B, with the arithmetic. Parse cost splits into lexing (42%, measured) and the rest (58%). Dead code is 72% of the source bytes (lexing) and 94.9% of the nodes (parse work), section 2.3. Preparse still lexes all source (42%). It keeps part of the parser work for syntax checks, scope and free variables, and drops node writes and arena growth. I assume it keeps 25% of the parser work (hypothesis; no number in the V8 post). Then:

- lex: 0.42 (unchanged)
- parser work for live code: 0.58 x 0.051 = 0.030
- parser work for dead code, preparse: 0.58 x 0.949 x 0.25 = 0.138
- parse again at first call: the called functions, 0.051 of the full cost = 0.051
- total: 0.42 + 0.030 + 0.138 + 0.051 = 0.64

So parse falls to 64% of today: 36% saved. With the preparser keeping 10% or 50% of the parser work the range is 0.57 to 0.77 (23 to 43% saved). On journeys: 6.5% x 0.36 = 2.3% (range 1.5 to 2.8%). On nodejs-org-home and crates-io-home, where parse is 29 to 31%, it is 10 to 11% (range 7 to 13%).

First walk. It visits every node once per realm. Lazy functions walk a function at its first call: 94.9% of nodes are not walked. First walk is 1.9% of journey instructions; the literal compile (section 6.5) is part of it. Saved: about 1.7% (estimate: 1.9% x 0.9, keeping a margin for the second walk of called functions).

Memory. AST plus cache over 45 journeys: 45.1 MB, 94.6% dead. B keeps 5.4% of it plus the stubs: 45.1 x 0.054 + 24,580 x 80 B = 2.4 + 2.0 = 4.4 MB. A 90% cut. On python-docs-search-results the AST is 22.2 MiB: at 90% dead (hypothesis, section 2.3) about 20 MiB saved, 17% of the 116 MiB peak.

C adds the transient tree: after bytecode, the tree of a called function is freed too. Nodes in memory at any time: the one function being compiled. That removes the last 5.4%. C depends on P3 and on a fallback for the places the bytecode lane does not cover (`with`, direct `eval`: `docs/decisions.md`, "Bytecode").

D is real (V8 and Hermes ship code caches and AOT bytecode), but it adds state outside the process. Section "Open questions".

The parse cache (`parse_cache.rs`) stays. Its cap can fall (section 8).

### 6.7 Garbage collection

Measured: GC is 1.5% inclusive on journeys. The collector runs only at depth 0.

| | A. Mark-sweep at depth 0 (current) | B. A: plus safepoints in the VM | C. Generational (nursery, copy) | D. Reference counting (QuickJS) |
|---|---|---|---|---|
| Data | slab with `dense` indirection | A, plus registers as exact roots | a young region, remembered set, write barrier | a count in every header, cycle pass |
| Algorithm | at statement boundaries | at loop back edges and calls, when the heap is above the threshold and no native frame holds an unrooted `Value` | minor collect often, major rarely | free at zero |
| Cost | O(live) | O(live) | O(survivors) | 4 B per object plus work on every copy of a handle |
| Work removed | | the "no collect inside one long statement" case: the 40,000-step concat loop, a JSON loop | mostly nothing: GC is 1.5% | |
| Risk | a loop that makes garbage inside one statement cannot be collected (measured: `Limit(Heap(Bytes))`) | the root set must be exact: natives that call back into JS hold `Value`s in Rust locals | write barrier everywhere; wide change | every `Value` copy changes a count; against the `Copy` enum |
| Estimate | | removes the concat failure mode (with ropes, it is moot); journeys: none | at most 1.5% (the whole GC cost) | not estimated |

Recommendation: B, inside P3. The register stack is a plain `Vec<Value>`, so its roots are exact and cheap. A native frame (a builtin that calls a JS callback, such as `Array.prototype.map`) stays a no-collect zone at first: the check changes from `depth == 0` to "no native frame on the Rust stack", which JS-to-JS calls no longer add. C is not worth 1.5%.

## 7. Recommended design: phases

Each phase is one or more PRs that land alone. Each measures the same way. "Gate" is in section 7.1.

### P1. Lazy functions, lazy regex literals, shared compiled regexes

Build:
- A preparse mode of the parser for function bodies: lex, check syntax and early errors, collect the free-variable set, push no nodes. A stub per function with its source span.
- Parse a function body at its first call into the arenas. First walk of a function at its first call.
- Regex: validate the pattern at load (a validate-only pass), compile at first evaluation, share the compiled program through a process-wide cache keyed by (pattern, flags). Replace the `/i` class scan by a range-based fold.
- A realm-independent per-script cache where possible (`Code` is rebuilt per realm today).

Moves (estimate): parse 6.5% to 4.2% (-2.3 points); first walk and literal compile 1.9% to 0.2% (-1.7 points); total about -4.0 points of journey instructions (range 3 to 5.5). AST plus cache memory -90%; python-docs-search-results peak about -17%. nodejs-org-home and crates-io-home about -10 points each.

Measured by: instruction sums over 48 journeys, 3 runs each, frozen BASE against NEW rebased on main; the dead-code counter from section 2.3 as a permanent test (functions called over functions parsed); `tools/js/cost_matrix.sh` (566 rows, no row above +0.5%); `tools/js/test262_check.sh` in tree, auto and byte modes; a new test that a syntax error inside a never-called function still fails at load; a new test that a captured variable of a skipped function is captured.

Limits it lets us remove or lower: `max_script_nodes` (8 M) counts live nodes only; the parse cache `MAX_BYTES` 32 MiB falls to about 4 MiB (32 MiB x 0.054 + stubs, estimate); the regex cache cleared at 256 entries and at every collect (`eval_builtins.rs:1245`) becomes a bytes-capped process cache.

### P2. Strings

Build: a static atom table for builtin and host names (built once per process); strings above the cut-off are not interned; rope nodes for `+` above the cut-off; flatten on first unit read, key use or regex exec; the two-class equality rule in `strict_equals`, `Map`, `Set`, `switch` and property keys.

Moves (estimate): intern 2.1% to 1.3% (-0.8 points); concat per step from O(n) to O(1); the 20,000-step loop from 13,830 to about 300 instructions per step.

Measured by: the 48 journeys, the cost matrix (`concat`, `intern`, `strings` rows), the micro-benchmark `concat_n`, a heap test at 100,000 steps, test262 `built-ins/String` and `language/expressions`.

Limits: the 1 GiB `max_bytes` is no longer reached by a concat loop. It stays as a host memory cap.

### P3. Register VM stage 2 (spike first)

Build, in order:
1. A spike: registers for uncaptured locals, fused `LtJmp` and `Inc`, constant operands. Measure `emptyloopfn` and `fib`. Gate: `loopfn` at least 4x and `fib` at least 3x over tree on the frozen micro-benchmarks, or stop and re-plan. This repeats R1 on the cases that matter.
2. Frames in one loop (B4): no Rust recursion for JS-to-JS calls; a frame stack in one `Vec<Value>`.
3. Boxed cells for captured variables.
4. GC safepoints at back edges and calls.
5. `Auto` becomes the default only when fallbacks are 0 on the corpora and test262 holds in all modes (the plan's B11).

Moves (estimate): loop iteration 410 to about 45 instructions (byte), `fib` call 1,895 to about 400; journeys -2.6 points; ramps -45% of instructions (-78% on the pure loop ramp). The calculation is in section 6.1.

Measured by: `eval_load` rows (`loop`, `loopfn`, `calls`, `fib`, `closures`, `objects`), the ramps in `/data/bare-tmp/bm`, the 48 journeys, the cost matrix, test262 in three modes, the keycost typing rows.

Limits: the step budget changes meaning. It counts back edges and calls (an interrupt budget), so 100 M buys several times the work. `max_call_depth` (100) and `max_statement_frames` (512) stop being Rust-stack limits for JS calls; the frame-stack bytes become the limit. The depth-0 rule of the collector goes.

### P4. The AST is transient

Build: compile a function to bytecode at its first call, from its freshly parsed body, then drop the nodes. A fallback re-parses the function for the tree walker when bytecode cannot run it (`with`, direct `eval`).

Moves (estimate): the remaining 5.4% of AST and cache bytes goes (about 2.4 MB over 45 journeys); the 3.7 MiB source copy `Program::into_owned` on python-docs-search-results can share the response buffer (to verify).

Measured by: peak RSS on the 48 journeys, the heavy-journey list in `prof/mem/report.md`, the byte-mode corpora with fallbacks = 0.

### P5. Compact objects

Build: a 32 to 40 B object record, inline slots in size-class slabs, the prototype in the shape, JSON objects built with shapes.

Moves (estimate): object slab 20.0 to 10.0 MiB, about -12 MiB on python-docs-search-results (-10% of its peak); about -0.7 points of journey instructions (hypothesis).

Measured by: `memjs` (1,000,000 objects: heap 99.3 MiB before), the heavy-journey memory list, the cost matrix.

### P6. Regex engine, data first

Build: first a per-pattern exec log (pattern, input length, steps, result) over the 48 journeys. Then, by the data: a literal-prefix or first-set scan with `memchr`-style search, a bounded backtracker with a visited bitmap, a PikeVM fallback after N backtracks, or a lazy DFA for `test`.

Moves: unknown until measured (exec is 2.2 to 2.55%). Bound: if exec halves, -1.2 points (hypothesis).

Limits: the 50 M regex step limit (`regexp.rs:154`) falls to the fallback threshold (V8: 50,000 backtracks) for patterns the fallback supports; `MAX_CLASS_WORK` is not needed once the fold is range-based.

### P7. 8-byte `Value` (optional)

Build: NaN-boxed `Value`; handles without `gn`, or a 16-bit generation. Decide after P5.

Moves (estimate): -4 to -5% of the heaviest journey's peak. Instruction effect: unknown.

### 7.1 How every phase is measured

| Gate | Tool | Rule |
|---|---|---|
| Journeys | `perf stat -e instructions:u`, 48 journeys, 3 runs each | frozen BASE against NEW rebased on main; report machine, run count, spread |
| Ramps | `/data/bare-tmp/bm` ramps | the same |
| Cost matrix | `tools/js/cost_matrix.sh BASE NEW` | no row above +0.5%, glibc and jemalloc |
| test262 | `tools/js/test262_check.sh` | no directory loses a passing test; tree, auto and byte |
| Memory | peak RSS, heap profile | the memory report list |
| Wall time | only on an idle machine | never under load |

### 7.2 Expected total

| Measure | After P1 + P2 | After P1 to P3 | After P1 to P6 |
|---|---|---|---|
| Journey instructions | -4.8 points (4.0 + 0.8) | -7.4 points | -8.1 to -9.3 points (P5 0.7, P6 up to 1.2) |
| JS ramp instructions | small | about -45% | about -45% |

| python-docs-search-results peak (116.3 MiB) | Saved | Share |
|---|---|---|
| P1: AST and cache of never-called code | about 20 MiB | 17% |
| P5: compact objects | about 12 MiB | 10% |
| P7: 8-byte values (optional) | about 4.7 MiB | 4% |
| Latin-1 strings (optional, section 6.4 option C) | about 8 MiB | 7% |
| Total | 32 MiB (P1 + P5), up to 44.7 MiB | 28%, up to 38% |

All of these are estimates. The journey points add up the phases and assume no overlap. P1 and P2 overlap a little in `intern` (string literal interning in the first walk), and the 0.8 point regex saving of section 6.5 is part of the 1.7 points of first walk in P1.

## 8. Limits this design lets us remove or lower

A limit that guards against a slow algorithm is a design smell (`CLAUDE.md`). After the phase in the second column, the limit is no longer needed or can be set by cost.

| Limit | Where | Phase | What happens |
|---|---|---|---|
| `max_script_nodes` 8 M, `max_script_source` 64 MiB | `eval.rs:132` | P1 | counts live nodes only; lower by the live share |
| Parse cache `MAX_BYTES` 32 MiB, `MAX_IDLE` 2 | `parse_cache.rs:26-30` | P1 | the cap falls to about 4 MiB (estimate) |
| Regex cache: cleared at every collect and at 256 entries | `eval_builtins.rs:1245`, `ext_prune` | P1 | a bytes-capped process-wide cache |
| `max_bytes` 1 GiB reached by a concat loop | `heap.rs:41` | P2, P3 | not reached by a loop; stays as a host cap |
| Step budget, 100 M, per node | `eval.rs:132` | P3 | per back edge and call; keep as a guard against infinite loops, add a wall-clock check (open question) |
| `max_call_depth` 100, `max_statement_frames` 512 | `eval.rs:132` | P3 | frame-stack bytes decide, not the Rust stack |
| Collect only at depth 0 | `eval.rs:1108` | P3 | collect at safepoints |
| Regex `max_steps` 50 M | `regexp.rs:154` | P6 | falls to the backtrack threshold of the fallback |
| `MAX_CLASS_WORK` | `regexp.rs` (`add_class`) | P1 | not needed with a range-based fold |

## 9. Risks

| Risk | Phase | Control |
|---|---|---|
| The free-variable set of a preparsed function is wrong (a missed capture) | P1 | a test per capture form; run test262 `language/` in tree, auto, byte; keep the full parser as an oracle: parse the function both ways in a debug build and compare |
| An early error inside a never-called function is lost | P1 | the preparser runs the same early-error checks; a negative test262 run |
| Parse and first walk of a function at its first call adds latency to a click handler | P1 | measure `type_load` and the keycost rows; a preparse pass does not move this cost to the first click for a handler that does little |
| Journeys are short, so dead code is overstated for long-running pages | P1 | the dead share is a permanent counter; re-measure on the typing page and the ramps |
| The register VM misses the target again | P3 | the spike gate before any merge |
| Two-class string equality breaks an identity assumption | P2 | find every `StrId` compare first (`grep`); property-based test: equal contents, equal result for `===`, `Map`, `Set`, `switch` |
| Safepoint GC roots are incomplete for natives | P3 | collect only with no native frame on the Rust stack; a stress mode that collects at every safepoint |

## 10. Not measured or not verified

- The dead-code share on python-docs-search-results (the journey failed in my run, no log). I assume 90%.
- Whether the 94.9% holds for longer sessions. I ran tree mode, so a byte-only call path is not counted.
- Which regex patterns cost the 2.2 to 2.55% of `exec_in` (no per-pattern log). The `add_class` cause (the 65,536 scan) is read from code, not profiled by pattern flags.
- The cost of a validate-only regex pass, and the preparser's share of parser work (I assumed 25%; range 10 to 50%).
- The instruction count of a fused register op (I assumed 15). The spike measures it.
- The share of the 45 MiB JS heap that is value arrays, and whether the 6.9 MiB of `PropMap::set` are JSON objects.
- Who allocates `for_in_keys` (0.98% of journey instructions).
- The decomposition of the remaining JS execution share outside `eval`, `heap` and `regexp` (1.2%).
- Wall time of anything (instruction counts only, as the rule says).
- V8 ropes and Hermes lazy compile (no source read). JSC LLInt details past the first half of the page. LuaJIT internals.
- Multiple runs of the micro-benchmarks (one run each; counts repeat to about 0.1%).
- The `concat_40000` failure is `Limit(Heap(Bytes))`: measured. The reason (1.6 GB of intermediate strings against 1 GiB: 40,000 steps x 40,000 / 2 x 2 B) is my arithmetic, not a heap trace.
- Four journeys failed an `expect` step in my instrumented run (crates-io-home, hn-new-past, mdn-reference, python-docs-search-results). I did not find why (hypothesis: the live sites changed; to verify).

## 11. Open questions for the owner

1. **Early errors for regex literals.** Today a bad pattern fails at `Interp::new` as `Unsupported`. Real engines raise a `SyntaxError` at parse time. For lazy compile I need a validate-only pass. May I add that pass (a few percent of compile), or may a bad pattern fail at first use?
2. **Early errors in uncalled functions.** The preparser keeps them (like V8). The alternative is a pure skip (brace matching): about 15 points cheaper in my estimate, but a syntax error in an unused function would not fail the script. Do we keep the spec behavior?
3. **A persistent code cache** (option D). It would make repeat visits near free, but it writes state to disk and changes determinism. Do we want it, now or later?
4. **Step budget meaning.** Move from "nodes" to "back edges and calls", plus a wall-clock check? Or keep node counts for tests that depend on the exact count?
5. **P3 gate.** Is "loopfn at least 4x and `fib` at least 3x over tree" the right stop rule? The old rule was 3x on `loop 10`, which now reads 1.90x.
6. **Order.** I put P1 and P2 before P3 because journeys gain more and the risk is lower. The ramps gain from P3. Do you want P3 first?
7. **Value size.** Do we try the 8-byte `Value` (P7) after P5, or keep 16 bytes (QuickJS does)?
8. **Latin-1 strings.** A wide API change for about 8 MiB on the heaviest journey (hypothesis). Do we do it only if memory is still above the goal after P5?
9. **Failed journeys.** Should I find out why four journeys failed in my private build before P1 starts?

## Appendix: files

All outside the repository, under `/data/bare-tmp/design/js/`:

| File | What |
|---|---|
| `src/` | private copy of the frozen source with logging (`dlog`, `Registry`, regex and script log lines, `design_sizes`) |
| `jrun.sh`, `out/j/*.log` | the 48-journey run with `DESIGN_LOG` |
| `agg.py`, `out/agg.json` | aggregation of regex and dead-code counts |
| `micro.sh`, `js/*.js`, `out/micro.tsv` | micro-benchmarks |
| `exec_ratio.sh`, `out/exec_ratio.tsv` | tree against byte on the `eval_load` rows |
| `incl_js.py`, `out/incl_js.json` | inclusive shares from the frame-pointer samples |
| `intern_callers.py` | callers of `Heap::intern` |

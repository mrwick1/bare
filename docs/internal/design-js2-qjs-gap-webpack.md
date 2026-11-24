# js2 and QuickJS instruction gap on webpack

## 1. Measurements

The frozen js2 run used 52.75 M instructions at scale 10. QuickJS used 38.22 M. OLD Auto used 47.22 M. The largest measured difference between js2 and QuickJS was 13.73 M instructions in bytecode compilation and setup. js2 spent another 5.38 M more in execution. QuickJS spent more in its initial parse and bytecode emission than js2 spent in both of its parse passes combined.

### Process totals

Each cell gives the minimum and range of three fresh processes in M user instructions. The machine was archMachine, Ryzen 9 5900HX, Linux, Rust 1.96.0, GCC 16.2.1 and perf 7.3.1. The event was `perf stat -e instructions:u`. The builds use the frozen revisions and launchers in the [engine decision benchmark](progress/2026-11-21-engine-decision-benchmark.md). js2 uses `05b520eb` and the script completion API. OLD uses `48ccf692`, `Exec::Auto` and no forced final collection. QuickJS is Bellard 2026-06-04, `-O2 -flto`, with the direct global `JS_Eval` launcher. Rust builds use locked release dependencies and debug symbols.

| Workload | js2 | QuickJS direct | OLD Auto |
| --- | ---: | ---: | ---: |
| Empty, source `0` | 0.7249–0.7260 | 1.0563–1.0569 | 2.0915–2.0942 |
| webpack 1 | 6.8559–6.8563 | 4.9913–4.9927 | 6.6997–6.7000 |
| webpack 10 | 52.7489–52.7570 | 38.2208–38.2260 | 47.2231–47.2245 |

Each minimum is within 0.1% of the matching report value. The input bytes match `eval_load webpack SCALE --source` after the checksum line and the output newline are removed. Each run returned the expected completion value. Scale 1 returned `3890,150,50`. Scale 10 returned `440005,1500,500`. The scale-10 SHA-256 is `7e2258759309af3db2a4208b4868a9fdb9b118887df39c6e48d545204e122532`. Its input is 53 KiB. It makes 500 distinct module wrappers and calls each once.

The starting worktree at `f29a583d` measured 6.7420–6.7441 M and 51.8493–51.8570 M on those two rows. The report used a scratch merge with different js2 code. Use the frozen measurements for the attribution below. Measure again on current main before the first implementation PR.

### Phase totals

Callgrind 3.25.1 with symbols. One profile per engine and input. Subtract the matching empty profile by phase. Units are M `Ir`. Rounding can change the sum. The profile totals before subtraction were 53.211 M for js2, 38.279 M for QuickJS and 48.313 M for OLD at scale 10. Use the hardware counts for process comparisons and the Callgrind counts for attribution.

| Scale | Engine | Initial parse | Reparse | Compile and code setup | Run | Other | Total net |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | js2 | 0.963 | 1.047 | 2.792 | 1.228 | 0.102 | 6.132 |
| 1 | QuickJS | 2.147 | 0 | 1.029 | 0.568 | 0.193 | 3.937 |
| 1 | OLD Auto | 0.893 | 0 | 1.890 | 1.511 | 0.397 | 4.691 |
| 10 | js2 | 8.484 | 8.888 | 22.718 | 11.480 | 0.883 | 52.452 |
| 10 | QuickJS | 20.326 | 0 | 8.989 | 6.100 | 1.789 | 37.204 |
| 10 | OLD Auto | 7.898 | 0 | 16.607 | 17.980 | 3.516 | 46.000 |

- For js2, use `parse_lazy` from `Lazy::script_in` and `parse_function` from the lazy compiler. Compile is the remainder of `script_code_in` and `compile_stub`. Run is `run_code` minus `compile_stub`. This includes cold prelude work that the fixture triggers. It excludes the fixed prelude and realm setup in the empty control.
- QuickJS emits its first bytecode form inside the parser. The parse column counts `js_parse_source_element` and its calls. The compile column counts `js_create_function` and its calls. Run counts `JS_EvalFunctionInternal`. Startup, file I/O and top-level teardown go in Other. The split between parse and compile describes each implementation. The sum of those columns is the comparable front-end cost.
- OLD parse counts `parse_script`. Compile counts function generation and finish plus the attributable script generation, scope walk and finish in `in_frame`. It is a lower bound. Some inline work remains in Run. Other includes the initialization and link pass in `Interp::new` and teardown. The comparison does not assign OLD's link pass to js2's lexer or emitter.

### Work per operation

The generator is in [eval_load.rs](../crates/js/examples/eval_load.rs). At scale 10, the module loop makes 1,498 calls to `__webpack_require__` and 500 calls to wrappers. The wrapper calls consist of one cold cache fill per module. There are 998 cache hits. The module path reads 2,496 keys from `cache` and 500 from `modules`. It reads `module.exports` 2,498 times, reads `.v` 1,498 times and writes `exports.v` 500 times. It writes 500 cache entries. There is one shared `require` function passed in as an argument. The other loop pushes 500 chunks and calls `onChunk` 500 times.

Set each loop bound to zero in turn. Keep the module table, all function source and the queue setup. The module-only control returns `440005,0,0`. The chunk-only control returns `0,1500,500`. The control with both bounds at zero returns `0,0,0`. Subtract the Run phase of this last control. Use source counts as divisors.

| Run cost | js2 Ir | QuickJS Ir |
| --- | ---: | ---: |
| Per module loop turn, includes cache hits and a wrapper call | 12,002 | 6,087 |
| Per require or wrapper entry, module-loop cost divided by 1,998 entries | 3,003 | 1,523 |
| Per chunk loop turn, includes bound calls and one array append | 7,331 | 4,609 |
| Per `exports.v` write in the measured slow named-store helper | 690 | 196 |
| Per `cache[id]` write in the general keyed-store helper | 1,485 | 402 |

The first three rows include dispatch, reads and associated allocation. The last two rows cover the called helper. They exclude the caller's dispatch and preparatory work. For the named store, js2 uses the difference between the module-only and both-zero `set_field_slow` edges. QuickJS uses its 500 wrapper edges to `JS_SetPropertyInternal`. For the keyed store, js2 counts the 500 VM calls to `set_prop_g`. QuickJS counts the 500 calls to `JS_SetPropertyValue`. These rows overlap the per-turn totals. Loop controls can change allocator and collector work. For example, QuickJS's two separate runtime increments sum to 5.35 M. Enabling both loops together gives 4.73 M.

Count executed js2 bytecode with the `op-count` feature and a copy of the completion API runner. Count QuickJS bytecode in a separate build with a counter at each computed-goto dispatch. Use the QuickJS dump to identify the functions and instructions. The counter builds do not supply the instruction timings. Each empty run dispatched three ops. A js2 wide prefix counts as part of its op.

| Bytecode measure | js2 | QuickJS |
| --- | ---: | ---: |
| webpack 1, all dispatches | 5,638 | 8,147 |
| webpack 10, all dispatches | 55,588 | 81,047 |
| webpack 10, dispatches per isolated module turn | 59.0 | 78.9 |
| webpack 10, dispatches per isolated chunk turn | 48.0 | 80.0 |

The scale-10 js2 run executed 507 `Lazy` ops, 502 `Closure`, 500 `DefElem`, 500 `SetFnName`, 500 `SetElem`, 7,005 `GetField` and 3,006 call ops. QuickJS executed 502 `fclosure` or `fclosure8`, 500 `set_name`, 500 `put_array_el`, 4,499 `get_field` and 3,002 call ops. The totals include supporting code on each engine's path. js2 dispatches fewer ops overall. Its general helpers and cold compilation cost more per unit of script work.

### React bundle front end

The input is the report's 314,192-byte `react-app.js` with SHA-256 `1992c72031ad2bad490a784172a2e9400b822cb0eb4d0e539f4a021af7c07400`. It is the typing fixture. The compile-only rows stop before script evaluation. Report RSS is 7.43 MiB for js2 and 6.53 MiB for QuickJS.

| Mode | Reproduced M instructions, min–max |
| --- | ---: |
| OLD full parse | 69.6766–69.6848 |
| js2 full parse | 63.4885–63.4913 |
| js2 eager compile | 323.2577–323.3019 |
| QuickJS compile only | 247.3241–247.3602 |

In the js2 eager profile, the initial parse costs 76.14 M Ir. The loader reparses 1,531 bodies at 73.23 M Ir and compiles them at 169.80 M Ir. Assembler work accounts for 56.06 M within compile. In QuickJS, parse plus first-form emission costs 130.02 M Ir. The final compiler costs 114.62 M Ir. These are the same two sources of extra work as on webpack: js2 reads bodies twice and spends more in code preparation. The 23% gap in eager compile occurs before script dispatch, function object creation or exports access. Browser load still defers many bodies. This row does not measure the first usable frame.

## 2. Root cause

### js2 discards work that webpack uses on its first calls

`parse_script_lazy` runs the parser on every wrapper and checks early errors. It collects per-function flags and capture spans. It then truncates the body arenas. `Loader::children` creates a `Code` stub and a pending record for each wrapper. `Lazy::compile` then finds the pending record, parses the body again, builds code, replaces the stub and starts the call. There were 507 first-use compiles at scale 10. 502 came from the source and five came from the prelude. The control with the module loop disabled removes 501 first-use compiles. The scale-1 run had 57. See [parse.rs](../crates/js2/src/parse.rs), [script.rs](../crates/js2/src/script.rs) and [codes.rs](../crates/js2/src/codes.rs).

The 8.89 M reparse cost is avoidable for wrappers that execute on load. The eager React API has the same reparse by construction. It uses the lazy parser even when the loader knows that all bodies must compile. js2 already skips nested function ranges when it reparses a parent. The duplicate reads in these profiles are the first and second reads of each function's own body.

### Code preparation is the largest phase deficit

`Fc::finish` passes instructions to `Asm::finish`, then translates line offsets, handlers and live roots into word PCs. In the webpack run, `Fc::finish` called `Asm::finish` for 6.58 M Ir across 509 functions, including startup. The assembler called `encode_in` 19,624 times at 5.34 M Ir. It copies instructions to a work vector, encodes them into a scratch vector to calculate widths, relaxes branch displacements and encodes again for the final buffer. The 500 wrappers have straight-line bodies. Their branch relaxation has no target to resolve. Width checks also parse operand letters from a string for each operand. The React profile attributes 18.31 M exclusive Ir to `Op::letter`, under both the code producer and its consumers. See [bytecode.rs](../crates/js2/src/bytecode.rs).

The loader then decodes completed words for flags it can know when it emits the instruction. Decoding alone costs 0.56 M Ir under the initial loader and 1.59 M under first-call compile. The full loader and cold pipeline cost includes per-function parameter and scope setup, atom tables, GC root maps and code publication. Root maps are required. `Fc::after` expands live-register bits to register numbers, and `Fc::finish` packs them into bits again. The profiles show which work exists. They do not establish the saving from removing each of these steps separately. See [compile.rs](../crates/js2/src/compile.rs), [interp.rs](../crates/js2/src/interp.rs) and [roots.rs](../crates/js2/src/roots.rs).

### Allocations and property access add a separate run cost

- The module table and cache are plain objects. `modules`, `cache` and `__webpack_require__` are script globals. The js2 wrappers use parameters and no heap context for the table. The require function looks up globals. QuickJS implements its references to these globals with `global_ref` entries. Its wrapper dumps show no closure variables. A variant wrapped in a sloppy IIFE made the table and cache lexical captures. The three-run mean was 54.04 M for js2 and 44.50 M for QuickJS. The original gap does not require lexical captures. The IIFE changes global and local loads as well as capture setup.
- The `Closure` op creates a fresh function cell for each literal. It stores a code index and the current context. Raw allocation is cheap in this case. The initialization control has 505 calls to `Obj::new` from the main VM loop at 0.041 M Ir, including five other cells. `SetFnName` calls `name_function` 500 times at 0.913 M Ir. It materializes own `length` and `name`, formats the numeric property key and sets the final name. QuickJS likewise creates each function and sets its numeric name. Its 502 nested `js_closure` calls cost 0.692 M Ir, and its 500 name definitions cost another 0.224 M. These scopes differ. The numbers identify a cold naming cost in js2. They do not support a general allocation or capture rewrite. See [interp_rt.rs](../crates/js2/src/interp_rt.rs) and [compile_expr.rs](../crates/js2/src/compile_expr.rs).
- A warm named read can use a js2 shape cache. Each wrapper has distinct `.v` and assignment sites. The sites run once. A new `exports` object has no own `v`. The first write must check the prototype and add a property. The scale-10 comparator saw roughly 690 Ir per slow helper versus 196 in QuickJS. The shared `.exports` read sites in require can warm. They should keep their cache. A charge for first-use checks and cache fill is different from a steady-state named-access gap.
- A plain object with integer keys does not take the array element fast path. The frozen js2 `get_prop_g` edge costs 2.06 M Ir for 2,997 calls from the VM. One is startup work inside the workload. Its `lookup_index_g` calls `idx_atom` at 1.20 M Ir. QuickJS's 2,996 general indexed reads call `JS_GetPropertyValue` for 0.317 M Ir. The small IDs already fit tagged QuickJS atoms. The named `.call` method still requires normal property semantics. The js2 `fn_call` native helper costs 0.051 M for 500 calls. The copying and interpreter entry around it have a wider scope.
- The queue is an independent runtime cost. js2 takes the generic indexed set path to check the prototype for an inherited setter before appending at a new index. The 500 `push` calls spend 0.726 M in `set_at` alone. The QuickJS fast-array append helper spends about 0.048 M including capacity growth. Each turn also performs two bound calls. The js2 `bound_call` helper accounts for 0.309 M over the 1,000 calls. See [bi_array.rs](../crates/js2/src/bi_array.rs) and [builtins.rs](../crates/js2/src/builtins.rs).

### Why OLD wins

OLD's 7.90 M full parse feeds both its tree walker and its compiler. OLD Auto compiles on the first call when a body contains a loop or a call. In the scale-10 profile it compiles 499 wrapper bodies on the nested path and two other functions. The wrapper that only assigns `1` runs once in the tree walker. OLD pays about 16.61 M for attributable compile work, including the attempted script compilation. It still avoids the second parse of all those wrappers. The js2 frontend costs 40.09 M versus 24.50 M on OLD. js2 saves about 6.50 M in execution and 2.63 M in Other. Those savings leave a 6.45 M net gap. OLD's empty startup costs another 1.37 M in the hardware runs. At scale 1 the fixed startup difference makes the hardware loss only 0.16 M.

A matched OLD Tree control took 5.6941–5.6957 M at scale 1 and 35.5117–35.5168 M at scale 10. It used the same launcher and checksum with `Exec::Tree`. It isolates the cost of choosing bytecode for this short execution. It also shows why a claim that OLD Auto already avoids compiling most wrappers would be incorrect. See [OLD tier policy](../crates/js/src/eval_bc.rs) and [OLD bytecode entry](../crates/js/src/bc_vm.rs).

### Local experiment

In a scratch copy of frozen js2, keep body AST nodes during the existing capture and early-error pass. Use those nodes for eager compile. Force the eager path for this exact webpack fixture to measure full coverage. Separately toggle one-pass encoding for bodies with no symbolic branch or labels. It keeps the operand checks, emitted words and map PCs.

| Process, three runs | Baseline M | Retain and compile M | Retain and encode once M |
| --- | ---: | ---: | ---: |
| webpack 1 | 6.8559–6.8563 | 5.8174–5.8196 | 5.6066–5.6076 |
| webpack 10 | 52.7489–52.7570 | 42.8787–42.9005 | 40.7331–40.7572 |
| React eager compile | 323.2577–323.3019 | 249.5099–249.5514 | 247.8396–247.8947 |

The first experiment removes 9.87 M hardware instructions on webpack 10 and 73.75 M on React. Callgrind confirms that webpack falls from 507 reparses to five prelude reparses. React falls from 1,531 to zero. This change also removes stub setup and entry work. The one-pass assembler removes another 2.15 M instructions on webpack and 1.67 M on React. In the webpack profile it lowers `Asm::finish` from 6.49 to 4.39 M Ir within the prototype. The combined build returns both webpack checksums. In the eager API, the words, line PCs, handlers, loop PCs, child slots, function length and name indexes, and live-root maps compare equal for all three inputs.

Retaining the entire React AST raises compile-only peak RSS from 7.28–7.36 to 9.74–10.01 MiB in three runs with the static `wait4` launcher. This measures the transient memory cost of the prototype. The browser selector therefore needs a node budget. The experiment used a fixture-specific trigger and did not validate a selection policy. I restored the scratch source after measurement.

## 3. Other engine designs

QuickJS starts with a `JSFunctionDef` for each function. Its parser emits bytecode and records scopes and atoms in that pass. `js_create_function` resolves variables and labels, computes stack size and packs bytecode, constants, variable definitions and closure descriptors into one allocation for each final bytecode function. `OP_fclosure` makes a distinct callable object. `js_closure2` allocates a reference array only if the bytecode has captures. The module wrappers need no such array. `JS_CallInternal` uses a stack VM with computed-goto dispatch in this build. `Function.prototype.call` can pass a pointer into the incoming argument window. Small integer keys fit tagged atoms. The ordinary property path uses shape descriptors. The array append has a fast-elements path. See the pinned `quickjs.c` functions `js_create_function`, `js_closure2`, `JS_GetPropertyValue`, `js_function_call` and `js_array_push`.

V8 documents a separate preparser and reuses scope metadata to skip previously checked nested functions. It parses and compiles recognizable likely IIFEs eagerly. The webpack fixture puts its wrappers in a table and invokes them indirectly. The documented IIFE heuristic alone would not select these wrappers. The precedent is to choose eager parsing when the syntax predicts immediate use and retain the lazy path for cold code. [V8 lazy parsing](https://v8.dev/blog/preparser)

## 4. Designs for js2

### A. Keep lazy body parsing and remove redundant bytecode work

Emit operand signedness and jump-field metadata as constants from the opcode list. Use one contiguous buffer for words and one for instruction PCs. If there is no symbolic jump, encode each instruction once. Keep the branch-relaxation path for control flow. Track code flags on emitted ops and validate them against the decoder in tests. Reuse the existing liveness bitset. Store a single word per safepoint when `nregs <= 64` and copy live words in larger frames. Keep `Code`, stable code slots and receiver feedback as they are.

The no-jump path is O(I + L) for I instructions and L labels, with O(I + L) output and no temporary instruction copy or wide flags. If jumping code still needs P relaxation passes, its time is O(P × I). Exact root maps occupy the same space and the same words at runtime. The compile path avoids a decode scan and operand string scans. Scope is two small PRs, about 150–250 lines plus focused tests. Hypothesis: 3–5 M saved on webpack 10 and 15–30 M on React, depending on saved encoding calls. This leaves 48–50 M in the frozen webpack row. Startup should be flat to lower because the metadata is static and compiler work falls. Closures and throws use the same maps and handlers. Browser risk is branch offsets and stack traces.

### B. Keep the first AST for eager functions and bound the cost for browser load

Use the same parser and function facts. Give each retained function a compact fact-to-AST index. The eager driver emits code from the retained AST and drops the arenas before execution. On script load, make a bounded eager set of simple, short leaf wrappers in static object literals. Use syntax and scope facts and a per-script budget. Limit the first selector to the literal shape the data supports, such as a table of small function values, then measure it on real bundles. Preserve the lazy driver when syntax or the budget excludes a function.

Keep retained body nodes in contiguous AST arenas. Store one index for each retained fact. Wait entries remain for other functions. Reuse the existing parent scope chain. Preserve code indexes, source offsets and early errors. Avoid a per-function source copy and a new permanent body representation. The existing parser and compiler still perform name resolution and syntax checks. Use direct indexes for retained bodies so lookup is O(1) per function. The retained subset removes a second O(B) read of B bytes. It adds O(N) transient node space for N retained nodes. Release the memory when the loader finishes. A bounded selection keeps the normal path from retaining a large dormant bundle on startup.

Scope is two PRs for the eager caller and the lazy selector, about 250–450 lines plus tests. Hypothesis: the combined design with A saves 10–14 M on webpack 10 and 65–85 M on React eager compile. A broad selector can increase startup CPU and retained code on real pages that use few modules. Keep `script::compile` eager by contract. Run sparse table and large unused bundle controls before enabling the browser policy. Check that exceptions from a call preserve their previous timing. Check closure identity, named function properties, scope environments, `super`, direct eval and already-loaded code after errors.

### C. Remove cold runtime work after the compiler fixes

Use a canonical keyed-property representation or an existing index-to-atom lookup for small integer keys. If tagged indices and name atoms share a property table, ensure that decimal strings and numeric keys hit the same descriptor. Keep property ordering, prototype traversal and dictionary shape rules. Avoid turning every object into an array. Keep sparse indices sparse. Separately, store the numeric property name as integer metadata and format it on observation. This keeps `length` and `name` lazy until observed. The ordinary method read of `.call` still uses its shape and prototype guards. Reuse an existing array prototype validity guard if the object and array work provides one. Use the inherited-setter path whenever the guard fails.

Small numeric-name metadata is one PR. A canonical key layout across shapes and dictionaries is several PRs. Expected keyed lookup stays amortized O(1) with the existing table strategy. A tagged descriptor key needs one word if it replaces the atom representation. A compatibility bridge can raise memory with duplicate entries. Hypothesis: this design could remove 1–3 M from webpack 10 after the frontend work, depending on what #908 and the object work already remove. It cannot affect React compile-only except for any change in atom or code metadata. The main risks are reflection, numeric key aliases, custom array prototypes, proxy and host properties and shape memory. Browser risk is higher for a key-layout change than for static name metadata.

## 5. Recommended design and implementation plan

Use A and B. The scratch build reached 40.73 M on webpack and 247.84 M on React. Measure current main first. Prefer removal of the second parse and the second encode over another general VM call path. Use C only for the residual work that a reprofile attributes to cold ordinary-object and array paths. Targets below are hypotheses on the frozen scale-10 reference. Record both scales, React eager compile and phase totals on the branch and its rebased baseline.

| Order | Scope and files | Correctness and cost gate | Cumulative target, webpack 10 / React eager compile |
| ---: | --- | --- | --- |
| 1 | Make assembler operand metadata static. Encode straight-line bodies once. `bytecode.rs` and focused bytecode tests. | Compare emitted words, label PCs and branch targets with the old assembler on a function corpus. Cover wide operands, code with labels and no branches, and crossing jumps. Gate on lower `Asm::finish` Ir. | 49–51 M / 300–313 M |
| 2 | Record flags at emit time. Copy exact live bits to root maps. `compile.rs`, `roots.rs`, `script.rs`, `interp.rs` and focused compiler tests. | Compare flags and root maps to the current output. Exercise GC at a call and a throwing accessor, handler reentry, `super`, spread, rest and argument-bearing stubs. Gate on lower code-preparation Ir. | 48–50 M / 293–308 M |
| 3 | Add a parser retention mode for eager compile. Feed preserved body IDs to the same compiler. `parse.rs`, `ast.rs`, `facts.rs`, `compile.rs`, `compile_expr.rs`, `script.rs`. | Compare eager bytecode, flags and spans to lazy compile after first call. Test nested scopes, non-simple parameters, eval and early errors. Gate on zero reparses in React eager compile and lower instructions. Measure transient RSS. | 48–50 M / 235–250 M |
| 4 | Select bounded short table leaves at normal script load. Use the same retained-body pipeline and keep the original path for other bodies. `parse.rs`, `script.rs`, `compile.rs`, `api_script.rs` and focused loader tests. | Check the real completion API in both webpack scales, a partly unused bundle and a table inside a lexical closure. Verify thrown errors, function metadata and closure identity. Gate on reduced lazy compiles and no increase outside the cost rule on cold bundles and browser rows. | 40–44 M / 235–250 M |
| 5 | If the runtime profile still supports it, keep a static numeric property name as integer metadata on the child code and stub. Format it on observation. Keep computed names on the existing path. `compile_expr.rs`, `script.rs`, `bytecode.rs` and `interp_rt.rs`. | Test name and length descriptors, own key order, redefinition, `bind`, and observation before and after first call. Check that setup savings exceed any added compile and first-property cost. | 39–44 M / 235–250 M |
| 6 | Reuse the measured ordinary-object or array fix from the object work. Prefer the narrower result from the module-only and chunk-only profiles. `interp_rt.rs` and `shapes.rs` for small integer keys, or `bi_array.rs` for guarded append. | Check decimal key aliases, sparse indices, proxies, host objects and inherited setters. Re-run module-only and chunk-only Run Ir, both original checksums and `objects`. | 38–43 M / 235–250 M |

For each PR, use the repository cost rule on touched rows with both allocators or the codegen-units control. Compare the unchanged empty, closures and throws controls and the relevant `interp_load` rows. Use a per-file `t` or narrow module tests and the js2 typecheck. Measure affected browser journeys on a replay or matched input: the React typing fixture, a script-heavy public journey and a local cold bundle with few called functions. Record instructions and peak RSS, plus readiness when the machine is idle. Record over-rule rows under the normal cost-debt process. The instruction targets above are estimates. A 38.2 M match depends on verified savings in both the compiler and the runtime.

## 6. Overlap with other work

- **Objects.** Reuse the same numeric-key and prototype work. #908 (`a37c75d8`) prevents atom creation on index descriptor misses. The frozen profile predates it. The cache and module keys often exist, and the frozen slow edge interns them on reads. Check #908 and #909 before changing atom handling. Credit their gains once. Avoid a separate index fast path for webpack.
- **Calls.** Share the cold call-entry profile and the bound and native tail-call evidence. Keep the compiler change independent of frame layout. The fixture has 500 calls to `Function.prototype.call` and 1,000 bound calls. Neither helper explains the compile-only React row.
- **Loop.** Share global load and dispatch measurements. The two outer loops have only 500 turns each. Direct Callgrind attribution assigns most of the gap to the frontend. Recheck dispatch after the frontend work lands.
- **Adaptive GC.** Reprofile after its merge. The loop controls change live objects and collection schedules. Keep the root-map and trigger tests separate. The parse and compile evidence comes from entry boundaries and does not require attributing the gap to collector policy.

The design does not report browser tests, test262, Windows, other allocators, idle wall time or a measured final implementation. The profiles, scratch scripts and raw samples are under `/data/bare-tmp/r22/gap-webpack/` on archMachine.

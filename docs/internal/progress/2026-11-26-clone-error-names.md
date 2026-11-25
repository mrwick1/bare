# 2026-11-26: A DataCloneError names the value that does not clone

Branch `t3/improve-datacloneerror-type`, merged with `origin/main` ebb46712.

## Why

The WhatsApp Web worker logs "An object could not be cloned." in bare. The text does not name the type, so the gap is hard to find.

## What changed

- `crates/js-api/src/clone.rs`: new variant `Refused::Named(String)`. It holds Chrome's name of the value. `Refused` is no longer `Copy`.
- `crates/vex/src/api_clone.rs`: the walk refuses an object of a kind that does not clone with `Refused::Named`. `clone_name` makes the name only on the refusal path. It reads only data properties on the prototype chain (V8 `GetDataProperty`). No getter and no proxy trap runs. A proxy gives the name of its target.
  - A platform object with an interface constructor: `<Interface> object` (Blink).
  - A script object whose `toString` is `Object.prototype.toString` and whose `constructor` is a named function: `#<Ctor>` (V8 `NoSideEffectsToString`).
  - Else `[object Tag]`, with `Symbol.toStringTag` or the built-in tag.
- `crates/vex/src/bi_proxy.rs`: `proxy_target_raw` reads the target without an error object.
- `crates/bare/src/browser_navigation_state.rs`: the host prints `<name> could not be cloned.`.
- The other refusals (functions, symbols, detached buffers, transfer-list items, a shared buffer the receiver cannot use) keep their text.

## Tests

- New `crates/bare/src/browser_clone_names_tests.rs`. It failed before the change ("An object could not be cloned.") and passes after it. The expected texts of the script objects are the output of Node 22 (V8): Promise, WeakMap, WeakSet, WeakRef, Proxy of an object, Proxy of an array, generator, Symbol wrapper, Promise with a `constructor` getter. Platform objects: Text, MediaQueryList. A test checks that no proxy trap runs. A `postMessage` over a `MessageChannel` gives the same text.
- Updated expectations: `browser_platform_tests.rs` (`platform_objects_are_not_plain_data_for_clone`), `browser_scheduler_tests.rs` (`the_transfer_list_is_checked_before_anything_moves`), `crates/vex/src/api_clone_tests.rs` (`clone_limits_and_refusals`).
- `t` passed: the files above, `browser_clone_tests.rs`, `browser_abort_tests.rs`, `browser_threaded_worker_tests.rs`, `bare-js-api` `clone_tests`, `bare-vex` `api_clone_shared_tests`, `builtins_more_tests`.
- Miri does not apply. The change is not in an unsafe module.

## Cost rows and test262

Machine archMachine, load average about 12 (other sessions). Base: release build of `origin/main` ebb46712. New: release build of this branch. Instructions in user mode, minimum of 3 runs per binary and allocator (`tools/js/instr.sh`). The rows are the Vex micro rows (`interp_load`) and the clone rows of `cost_rows.txt`.

| Row | glibc base | glibc new | change | jemalloc base | jemalloc new | change |
| --- | --- | --- | --- | --- | --- | --- |
| `interp_load loopfn 1000000` | 261293460 | 261293269 | -0.0001% | 264663986 | 264664123 | +0.0001% |
| `interp_load calls 1000000` | 549342178 | 549342353 | +0.0000% | 552713354 | 552712879 | -0.0001% |
| `interp_load fib 30` | 1020975942 | 1020975855 | -0.0000% | 1024377284 | 1024377223 | -0.0000% |
| `interp_load closures 200000` | 358803616 | 358803705 | +0.0000% | 362193351 | 362193312 | -0.0000% |
| `interp_load objects 200000` | 142399671 | 142399147 | -0.0004% | 145769669 | 145769795 | +0.0001% |
| `web_apis_load clone 1` | 145017627 | 145067804 | +0.0346% | 149623472 | 149665774 | +0.0283% |
| `web_apis_load clone 10` | 1409440533 | 1410125656 | +0.0486% | 1421635730 | 1422326555 | +0.0486% |

No row is over +0.5%. Main then moved to 80a8073c. That change touches no Vex, js-api or clone code, so the rows did not run again. Hypothesis: the clone rows grow by under 0.05% because of code layout. The success path runs the same code.

test262 (`tools/js/test262_js2.sh -j 8`, pinned subset e4fbf577) on both trees: 31409 passed, 429 failed, 8028 skipped. The per-test results are equal.

## Gaps found, not fixed

- Vex has no global `Iterator`, so `Iterator.prototype.constructor` is not an accessor. An iterator gives `#<Object>`. Chrome gives `[object Array Iterator]`.
- A `div` in bare has the `HTMLElement` prototype. Chrome names it `HTMLDivElement`.
- Chrome clones `DOMRect`. bare refuses it (now `DOMRect object could not be cloned.`, hypothesis: not checked by a test).
- Chrome says "A MessagePort could not be cloned because it was not transferred." for a port that is not in the transfer list. bare says `MessagePort object could not be cloned.`.

## Not tested

Chrome itself (the platform-object form is from Blink source, not from a run), the WhatsApp worker, Windows, the full suite, the full cost matrix.

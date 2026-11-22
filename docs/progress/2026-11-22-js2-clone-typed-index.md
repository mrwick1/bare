# 2026-11-22 — Read typed array indices in host prototype chains

The React Docs journey reaches CodeMirror after structured clone works. Its parser reads a Uint16Array until it finds 65535. The host lookup returned undefined for every index when the prototype chain contained a lazy host object. The parser then ran past the end of the table.

The host walk now uses the typed array's integer-indexed lookup before it continues down the chain. A canonical numeric key that does not name an element ends the lookup. Both property reads and `in` use this rule.

## Checks

The new host test failed on main. It covers a typed array, an object that inherits from it, and canonical numeric keys that miss. All 22 host API tests pass. `cargo check --locked -p bare-js2 -p bare` passes. Test262 TypedArrayConstructors internal Get and HasProperty have 19 passes, 0 failures and 12 skips on both builds. No passing files were lost.

The structured-clone build without this fix timed out in React Docs. With both fixes, each engine passes the same recorded journey three times with 13 replies and 501,473 bytes. On archMachine with the virtual clock, the minimum and range are 5,218.945 [5,218.945–5,227.512] million instructions on js2 and 6,694.844 [6,694.844–6,801.156] million on OLD. Peak RSS is 87,040 [86,276–87,184] KiB on js2 and 124,416 [122,964–124,604] KiB on OLD.

## Cost

The five `interp_load` rows stayed within 0.002% under glibc and jemalloc. The four `binary_load scan` and `indexof` rows stayed within 0.008%. Base code is `862ab0c3`. Raw data is under `/data/bare-tmp/r22/js2-clone`.

Local checks omit the full workspace suite, Windows and Miri.

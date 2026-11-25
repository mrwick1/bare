# Tests: `createDocument` exists

#1108 added `DOMImplementation.prototype.createDocument`. `browser_tests::dom_implementation_interface` still expected it to be `undefined` and failed on main. The test now expects a function.

- `cargo test -p bare --lib browser_tests::dom_implementation_interface`: pass.
- No production code changed. No cost rows ran.

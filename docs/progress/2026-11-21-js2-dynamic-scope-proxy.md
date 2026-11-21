# Dynamic scope and Proxy in js2

## Name resolution

The parser records direct calls to `eval` and the scope of each `with`.
Keep register and context-slot instructions where no dynamic environment can shadow a name.
Capture local bindings that a direct eval can read, including bindings in enclosing functions.
Reuse the context chain for object environments and for extra bindings in a sloppy eval.
The compiler marks each dynamic scope in the chain.
Resolve a name across those scopes from the innermost scope out.
Keep the resolved reference across a right-hand side that can change the scope object.
Use the same reference for reads, writes, deletes, and the receiver of a call.
Check `Symbol.unscopables` in a with environment.
Strict eval keeps new var bindings in its own scope.

A realm-wide name table would add lookups to ordinary code and would need frame identity to keep recursion correct.
A separate interpreter for affected functions would duplicate control flow, exception handling, and collection rules.
Context slots keep one interpreter and put the extra work at the affected references.

## Proxy

Use a free object kind in `F_KIND`.
Store the target and handler in traced slots.
Give proxies a shape that the property inline cache cannot install.
Use the existing slow path to look up the trap, call it with the handler as receiver, and check the result against the target.
Forward operations with no trap to the target.
Stop ordinary prototype cache construction at a proxy.
A separate cell kind would also keep proxies out of the shape cache.
It would require changes to each path that accepts an object value, including rooting and conversion.

## Checks

Run the listed test262 regressions first and use the pinned baseline to check for lost passes.
Count user instructions for the interpreter workloads, startup, and parsing on both allocators.
Use the minimum of three runs on each build.

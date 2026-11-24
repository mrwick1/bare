// Not built in Vex: `new` of a bound function, the error of an assignment to an undeclared name in strict code, the `cause` option.
print((function () { try { new (function () {}.bind())(); return 'none'; } catch (e) { return e.name; } })());
print((function () { try { (function () { 'use strict'; undeclared = 1; })(); return 'none'; } catch (e) { return e.name; } })());
print(new Error('m', { cause: 1 }).cause);

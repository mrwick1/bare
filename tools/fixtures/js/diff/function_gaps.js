// Function.length is not built, and `this` of a sloppy function call is undefined (it must be the global object).
function add(a, b) { return a + b; }
print(add.length);
print((function () { return this === undefined; })());

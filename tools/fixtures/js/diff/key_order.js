// The order of integer keys: the spec lists them first, in ascending order. Vex keeps the order of insertion (S3).
var n = { 2: 'b', 1: 'a', x: 'c' }; print(Object.keys(n).join());
var keys = []; for (var p in { a: 1, b: 2, 1: 3 }) keys.push(p); print(keys.join());

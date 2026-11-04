// One script of the multi-script seam test. `@` is replaced by a tag (A, B). Every copy has the same shape, so the
// node ids of its functions and expressions collide with the other copy's. Only our own tiny program.
var closed@ = 0;
var src@ = { [Symbol.iterator]() { var i = 0; return { next() { i++; return { value: '@' + i, done: i > 4 }; }, return() { closed@++; return {}; } }; } };
function map@(x, i) { return x + '/' + i; }
var spread@ = { length: 2, 0: '@0', 1: '@1' }; spread@[Symbol.isConcatSpreadable] = true;
var sp@ = {}; Object.defineProperty(sp@, Symbol.species, { get: function () { return undefined; } });
var arr@ = [3, 1, 2]; Object.defineProperty(arr@, 'constructor', { value: sp@ });
function cmp@(a, b) { return a - b; }
var ent@ = { [Symbol.iterator]() { var i = 0; return { next() { i++; return { value: ['k' + i, '@'], done: i > 2 }; }, return() { closed@ += 100; return {}; } }; } };
'@';

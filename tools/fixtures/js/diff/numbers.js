// Number to string, conversions and operators.
var xs = [0, -0, 1, -1, 0.1, 0.1 + 0.2, 123456789012345680000, 2147483648, -2147483649, 4294967296, NaN, Infinity, -Infinity];
for (var i = 0; i < xs.length; i++) print(String(xs[i]), xs[i] | 0, xs[i] >>> 0, Math.floor(xs[i]), Math.round(xs[i]));
print(7 % 3, -7 % 3, 7 % -3, 5.5 % 2, 2 ** 10, 2 ** -1, (-8) ** (1 / 3));
print(parseInt('42px'), parseInt('0x1f'), parseInt('  -7'), parseFloat('3.14abc'), parseInt('z', 36), Number('  12  '), Number(''), Number('1e3'), Number('abc'));
print(isNaN('x'), isFinite('12'), 1 / 0, -1 / 0, 0 / 0);
print((255).toString(16), (255).toString(2), (0.5).toString(2), (-255).toString(36));
print(1 << 31, 1 << 32, -1 >> 1, -1 >>> 1, ~5, 5 & 3, 5 | 3, 5 ^ 3);
print(0.1 * 3, 1 / 3, 100 / 3, 1e300 * 1e10, 9007199254740993);

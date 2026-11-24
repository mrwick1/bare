// Array.of and Array.prototype.flat are not built in Vex yet (docs/progress.md, test262 causes).
print(Array.of(7).join());
print([1, [2, [3, [4]]]].flat(2).length);

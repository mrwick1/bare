function mk(n) { var c = n; return function() { c = (c * 7 + 1) % 10007; return c; }; }
                     var fs = []; for (var i = 0; i < 5000; i++) fs.push(mk(i));
                     var s = 0; for (var j = 0; j < 5000; j++) s += fs[j]();
                     function first() { return fs[0](); } s

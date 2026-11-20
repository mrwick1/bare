function f(a, b) { return (a * 31 + b) % 65521; } var s = 1; for (var i = 0; i < 100000; i++) s = f(s, i); s

var c = 0; for (var i = 0; i < 20000; i++) { try { if (i % 3 == 0) throw new Error('e' + i); c += 1; } catch (e) { c += e.message.length; } finally { c++; } } c

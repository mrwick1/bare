var o = {}; for (var i = 0; i < 20000; i++) o['k' + (i % 997)] = i;
                     var s = 0; for (var k in o) s += o[k];
                     var a = []; for (var j = 0; j < 20000; j++) a.push(j % 13);
                     var t = 0; a.forEach(function(v) { t += v; }); s + ',' + t

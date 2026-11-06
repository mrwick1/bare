// Shared by tools/ui/chrome_xhr.mjs (headless Chrome) and the bare test: the log `out`, the event recorder and `done`.
var out = [], finished = false;
var EV = ['readystatechange', 'loadstart', 'progress', 'load', 'error', 'abort', 'timeout', 'loadend'];
function path(u) { return String(u).replace(/^https?:\/\/[^\/]*/, ''); }
function rec(x, tag) {
  tag = tag || '';
  var t = tag ? x.upload : x;
  EV.forEach(function (n) {
    t.addEventListener(n, function (e) {
      if (n === 'readystatechange') out.push(tag + n + ':' + x.readyState);
      else out.push(tag + n + '(' + e.loaded + '/' + e.total + (e.lengthComputable ? '!' : '') + ')');
    });
  });
}
function done(x) { x.addEventListener('loadend', function () { setTimeout(function () { finished = true; }, 30); }); }
function after(ms) { setTimeout(function () { finished = true; }, ms); }
function err(f) { try { f(); return 'ok'; } catch (e) { return e.name + ':' + e.code; } }
function hdrs(x) { var s = x.getAllResponseHeaders(); return JSON.stringify(s.split('\r\n').filter(function (l) { return !/^(date|connection|keep-alive):/.test(l); }).join('\r\n')) + ',' + s.endsWith('\r\n'); }

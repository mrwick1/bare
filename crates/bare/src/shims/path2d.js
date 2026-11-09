// `Path2D`: a constructor whose path methods do nothing. No canvas reads a path. Made at the first read of `Path2D` (`browser_shims.rs`).
(function () {
  'use strict';
  function Path2D(path) {
    if (!(this instanceof Path2D)) throw new TypeError("Failed to construct 'Path2D': Please use the 'new' operator, this DOM object constructor cannot be called as a function.");
  }
  ['addPath', 'arc', 'arcTo', 'bezierCurveTo', 'closePath', 'ellipse', 'lineTo', 'moveTo', 'quadraticCurveTo', 'rect', 'roundRect'].forEach(function (n) {
    Object.defineProperty(Path2D.prototype, n, { value: function () {}, writable: true, enumerable: true, configurable: true });
  });
  Object.defineProperty(Path2D.prototype, Symbol.toStringTag, { value: 'Path2D', configurable: true });
  Object.defineProperty(globalThis, 'Path2D', { value: Path2D, writable: true, configurable: true });
})();

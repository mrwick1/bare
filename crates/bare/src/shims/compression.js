// `CompressionStream` and `DecompressionStream` (Compression Standard), over `TransformStream`. Made at the first read of one of the
// names (`browser_shims.rs`). The source gives a function; the host calls it with `codec(mode, format, bytes)`, which compresses
// (mode 0) or decompresses (mode 1) all the bytes at once, or throws a TypeError.
// The chunks are kept until the writable side closes, then the codec runs once and the output is one chunk. The standard lets the
// output come in any number of chunks. Limit: a stream gives no output before its writable side closes.
(function () {
  'use strict';
  var U8 = Uint8Array, AB = ArrayBuffer, TE = TypeError, FORMATS = ['gzip', 'deflate', 'deflate-raw'], S = new WeakMap();
  function brand(o, who) {
    var s = S.get(o);
    if (!s) throw new TE("Failed to execute '" + who + "': Illegal invocation");
    return s;
  }
  return function install(codec) {
    function make(self, mode, name, format) {
      if (format === undefined) throw new TE("Failed to construct '" + name + "': 1 argument required, but only 0 present.");
      var f = FORMATS.indexOf(String(format));
      if (f < 0) throw new TE("Failed to construct '" + name + "': Unsupported compression format: '" + String(format) + "'");
      var parts = [], total = 0;
      var ts = new TransformStream({
        transform: function (chunk) {
          var b;
          if (chunk instanceof AB) b = new U8(chunk.slice(0));
          else if (AB.isView(chunk)) b = new U8(chunk.buffer, chunk.byteOffset, chunk.byteLength).slice();
          else throw new TE("Failed to execute 'write' on '" + name + "': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'.");
          parts.push(b);
          total += b.length;
        },
        flush: function (c) {
          var all = new U8(total), at = 0;
          for (var i = 0; i < parts.length; i++) { all.set(parts[i], at); at += parts[i].length; }
          parts = [];
          var out = codec(mode, f, all);
          if (out.length) c.enqueue(out);
        }
      });
      S.set(self, ts);
    }
    class CompressionStream {
      constructor(format) { make(this, 0, 'CompressionStream', format); }
      get readable() { return brand(this, 'CompressionStream').readable; }
      get writable() { return brand(this, 'CompressionStream').writable; }
    }
    class DecompressionStream {
      constructor(format) { make(this, 1, 'DecompressionStream', format); }
      get readable() { return brand(this, 'DecompressionStream').readable; }
      get writable() { return brand(this, 'DecompressionStream').writable; }
    }
    [CompressionStream, DecompressionStream].forEach(function (C) {
      Object.defineProperty(C.prototype, Symbol.toStringTag, { value: C.name, configurable: true });
      Object.defineProperty(globalThis, C.name, { value: C, writable: true, configurable: true });
    });
  };
})()

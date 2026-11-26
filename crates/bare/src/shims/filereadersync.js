// `FileReaderSync` (File API, "The FileReaderSync API"). Workers only: `browser_shims.rs` makes it at the first read of the name in a
// realm with `DedicatedWorkerGlobalScope`. The source gives a function; the host calls it with `bytes(blob)`, which gives a new
// ArrayBuffer with the bytes of a Blob, or undefined for any other value. The bytes of a bare Blob are in memory, so a read never fails.
(function () {
  'use strict';
  return function (bytes) {
    function read(name, blob) {
      var buf = bytes(blob);
      if (buf === undefined) throw new TypeError("Failed to execute '" + name + "' on 'FileReaderSync': parameter 1 is not of type 'Blob'.");
      return buf;
    }
    function latin1(buf) {
      var b = new Uint8Array(buf), s = '', i;
      for (i = 0; i < b.length; i += 8192) s += String.fromCharCode.apply(null, b.subarray(i, i + 8192));
      return s;
    }
    class FileReaderSync {
      readAsArrayBuffer(blob) { return read('readAsArrayBuffer', blob); }
      readAsBinaryString(blob) { return latin1(read('readAsBinaryString', blob)); }
      readAsText(blob, enc) {
        var buf = read('readAsText', blob), dec;
        // An unknown encoding label reads as UTF-8, as the standard says.
        try { dec = new TextDecoder(enc); } catch (e) { dec = new TextDecoder(); }
        return dec.decode(buf);
      }
      readAsDataURL(blob) {
        var s = latin1(read('readAsDataURL', blob));
        return 'data:' + (blob.type || 'application/octet-stream') + ';base64,' + btoa(s);
      }
    }
    Object.defineProperty(FileReaderSync.prototype, Symbol.toStringTag, { value: 'FileReaderSync', configurable: true });
    Object.defineProperty(globalThis, 'FileReaderSync', { value: FileReaderSync, writable: true, configurable: true });
  };
})()

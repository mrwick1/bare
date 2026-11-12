// Read a ReadableStream body to its end: `text()`, `json()`, `arrayBuffer()`, `blob()` and `formData()` of `new Response(stream)`
// (`browser_fetch.rs`). Made at the first such call. The stream is read with the host reader, so the stream limits hold. The total
// is checked at each chunk, so at most `max` bytes are kept. The bytes then go into a new byte Response, and its method makes the
// value, so the decoding and the limits are the ones of a byte body.
(function () {
  'use strict';
  var P = Promise, U8 = Uint8Array, Resp = Response, TE = TypeError, proto = Response.prototype;
  var how = { text: proto.text, json: proto.json, arrayBuffer: proto.arrayBuffer, blob: proto.blob, formData: proto.formData };
  return function drain(stream, max, kind, type) {
    var reader = stream.getReader(), parts = [], total = 0;
    return new P(function (resolve, reject) {
      function fail(e) {
        reader.cancel(e).then(undefined, function () {});
        reject(e);
      }
      function step(r) {
        if (r.done) {
          var all = new U8(total), at = 0;
          for (var i = 0; i < parts.length; i++) { all.set(parts[i], at); at += parts[i].length; }
          parts = null;
          var init = type ? { headers: { 'content-type': type } } : undefined;
          resolve(how[kind].call(new Resp(all, init)));
          return;
        }
        var c = r.value;
        if (!(c instanceof U8)) return fail(new TE('Response body stream gave a chunk that is not a Uint8Array'));
        total += c.length;
        if (total > max) return fail(new TE('Response body stream is over the limit of ' + max + ' bytes'));
        parts.push(c);
        reader.read().then(step, reject);
      }
      // The first read is synchronous, so the stream is disturbed (bodyUsed) when the method returns.
      reader.read().then(step, reject);
    });
  };
})()

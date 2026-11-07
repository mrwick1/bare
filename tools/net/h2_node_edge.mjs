// A test server for the edge cases of the HTTP/2 client, built on Node's `http2` (nghttp2): an oracle that is not our code.
// usage: node h2_node_edge.mjs PORT CERT.pem KEY.pem [OPTIONS_JSON]
// Options: maxStreams (SETTINGS_MAX_CONCURRENT_STREAMS), padding ("max" or "aligned"), idleMs (close a session after this idle time:
// GOAWAY, then close), idleKill (destroy the socket with no GOAWAY instead), settings (more http2 settings).
// Routes (the stream API, so the server controls every frame):
//   /plain?n=N            N bytes of a pattern with a content-length (padded when the option is set)
//   /priority?n=N         the same, after a PRIORITY frame for the stream
//   /interim?kind=K       100, 103 or multi (100, 103, 103) before a 200 with "final"
//   /trailers?n=N         N bytes, then trailers (x-sum)
//   /gzip /br /deflate    ?n=N: N bytes of text with that content-encoding (content-length set); &stream=1: no content-length
//   /empty?status=S       S (204 or 304) with content-encoding: gzip and no body; a HEAD of /gzip does the same
//   /hugehdr?n=K          K headers of 1000 random characters (a header block over 16 KiB: CONTINUATION frames)
//   /settings?k=v&...     send SETTINGS now: max (streams), window, frame; then "ok"
//   /goaway?last=L        GOAWAY(NO_ERROR, last stream L; 0 means this stream - 2) and then the answer "ok"
//   /slow?ms=M&n=N        N bytes after M ms
//   /trickle?parts=P&gap=G  P parts of 100 bytes, G ms apart
//   /upload               JSON: the bytes of the body, and their sha256
//   /conns                JSON: connections so far, streams so far, the most streams open at once
import http2 from "node:http2";
import zlib from "node:zlib";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";

const [port, cert, key, optJson = "{}"] = process.argv.slice(2);
const opt = JSON.parse(optJson);
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const bytes = (n) => {
  const b = Buffer.alloc(n);
  for (let i = 0; i < n; i++) b[i] = i % 251;
  return b;
};
const text = (n) =>
  Buffer.from(
    "the quick brown fox jumps over the lazy dog\n"
      .repeat(Math.ceil(n / 44))
      .slice(0, n),
  );
let conns = 0,
  streams = 0,
  open = 0,
  peak = 0;

const server = http2.createSecureServer({
  key: readFileSync(key),
  cert: readFileSync(cert),
  paddingStrategy:
    opt.padding === "max"
      ? http2.constants.PADDING_STRATEGY_MAX
      : opt.padding === "aligned"
        ? http2.constants.PADDING_STRATEGY_ALIGNED
        : http2.constants.PADDING_STRATEGY_NONE,
  settings: {
    ...(opt.maxStreams ? { maxConcurrentStreams: opt.maxStreams } : {}),
    ...opt.settings,
  },
  maxSendHeaderBlockLength: 1 << 20,
});
server.on("session", (s) => {
  conns++;
  s.on("error", () => {});
  if (opt.idleMs)
    s.setTimeout(opt.idleMs, () => (opt.idleKill ? s.destroy() : s.close()));
});
server.on("stream", async (stream, h) => {
  streams++;
  peak = Math.max(peak, ++open);
  stream.on("close", () => open--);
  stream.on("error", () => {});
  const url = new URL(h[":path"], "https://x");
  const q = (k) => url.searchParams.get(k);
  const n = Number(q("n") ?? 0);
  const head = h[":method"] === "HEAD";
  const reply = (headers, body) => {
    if (stream.destroyed) return;
    stream.respond(
      { ":status": 200, ...headers },
      { endStream: head || !body },
    );
    if (body && !head) stream.end(body);
  };
  const chunks = [];
  for await (const c of stream) chunks.push(c);
  const upload = Buffer.concat(chunks);
  const json = (o) =>
    reply(
      { "content-type": "application/json" },
      Buffer.from(JSON.stringify(o)),
    );
  const coded = async (name, fn) => {
    const raw = text(n);
    if (q("stream")) {
      stream.respond(
        { ":status": 200, "content-encoding": name },
        { endStream: head },
      );
      if (head) return;
      const z = fn.stream();
      z.pipe(stream);
      for (let i = 0; i < raw.length; i += 1000) {
        z.write(raw.subarray(i, i + 1000));
        await wait(1);
      }
      return z.end();
    }
    const body = fn.sync(raw);
    reply(
      {
        "content-encoding": name,
        "content-length": body.length,
        "content-type": "text/plain",
      },
      body,
    );
  };
  switch (url.pathname) {
    case "/plain":
      return reply(
        { "content-length": n, "content-type": "application/octet-stream" },
        bytes(n),
      );
    case "/priority":
      stream.priority({ parent: 0, weight: 200, exclusive: false });
      return reply({ "content-length": n }, bytes(n));
    case "/interim": {
      const kind = q("kind");
      if (kind === "100" || kind === "multi")
        stream.additionalHeaders({ ":status": 100 });
      if (kind === "103" || kind === "multi")
        stream.additionalHeaders({
          ":status": 103,
          link: "</a.css>; rel=preload; as=style",
        });
      if (kind === "multi")
        stream.additionalHeaders({
          ":status": 103,
          link: "</b.js>; rel=preload; as=script",
        });
      return reply({ "content-length": 5 }, Buffer.from("final"));
    }
    case "/trailers":
      stream.respond(
        { ":status": 200, "content-length": n },
        { waitForTrailers: true },
      );
      stream.on("wantTrailers", () => stream.sendTrailers({ "x-sum": "42" }));
      return stream.end(bytes(n));
    case "/gzip":
      return coded("gzip", { sync: zlib.gzipSync, stream: zlib.createGzip });
    case "/br":
      return coded("br", {
        sync: zlib.brotliCompressSync,
        stream: zlib.createBrotliCompress,
      });
    case "/deflate":
      return coded("deflate", {
        sync: zlib.deflateSync,
        stream: zlib.createDeflate,
      });
    case "/empty": {
      const status = Number(q("status") ?? 204);
      return stream.respond(
        { ":status": status, "content-encoding": "gzip", etag: '"x"' },
        { endStream: true },
      );
    }
    case "/hugehdr": {
      const h2 = { "content-length": 2 };
      for (let i = 0; i < n; i++)
        h2[`x-big-${i}`] = randomBytes(750).toString("base64").slice(0, 1000);
      return reply(h2, Buffer.from("ok"));
    }
    case "/settings": {
      const s = {};
      if (q("max")) s.maxConcurrentStreams = Number(q("max"));
      if (q("window")) s.initialWindowSize = Number(q("window"));
      if (q("frame")) s.maxFrameSize = Number(q("frame"));
      stream.session.settings(s);
      return reply({ "content-length": 2 }, Buffer.from("ok"));
    }
    case "/goaway": {
      const last = Number(q("last") ?? 0) || stream.id - 2;
      stream.session.goaway(0, last);
      return reply({ "content-length": 2 }, Buffer.from("ok"));
    }
    case "/slow":
      await wait(Number(q("ms") ?? 0));
      return reply({ "content-length": n }, bytes(n));
    case "/trickle": {
      stream.respond({ ":status": 200 });
      for (let i = 0; i < Number(q("parts")); i++) {
        stream.write(bytes(100));
        await wait(Number(q("gap") ?? 0));
      }
      return stream.end();
    }
    case "/upload":
      return json({
        bytes: upload.length,
        sha256: createHash("sha256").update(upload).digest("hex"),
      });
    case "/conns":
      return json({ conns, streams, peak });
    default:
      stream.respond({ ":status": 404 }, { endStream: true });
  }
});
server.listen(Number(port), "127.0.0.1", () => console.log("listening"));

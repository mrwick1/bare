// A test server for the HTTP/2 client, built on Node's `http2` (nghttp2): an oracle that is not our code.
// usage: node h2_node_server.mjs PORT CERT.pem KEY.pem [DELAY_MS]
// It speaks h2 and http/1.1 (ALPN), so the same server answers the client before and after HTTP/2.
// Routes:
//   /echo            JSON: method, the headers that the server decoded, the number of body bytes
//   /headers?n=K     K answers with the same big set of headers (every name of the HPACK static table), to fill the dynamic table
//   /bytes?n=N       N bytes of a pattern
//   /upload          JSON: the number of bytes of the body, and their sha256
//   /conns           JSON: the number of TLS connections so far
//   /asset/N         about 2 KiB of text, after DELAY_MS
//   /login           an html page with `assets` scripts and styles (`?assets=N`, default 40)
import http2 from 'node:http2';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const [port, cert, key, delay = '0'] = process.argv.slice(2);
const wait = ms => new Promise(r => setTimeout(r, ms));
let conns = 0;
const names = ['accept-charset','accept-language','accept-ranges','accept','access-control-allow-origin','age','allow','cache-control','content-disposition','content-language','content-location','content-range','etag','expect','expires','from','if-match','last-modified','link','location','max-forwards','proxy-authenticate','range','referer','refresh','retry-after','server','set-cookie','strict-transport-security','vary','via','www-authenticate'];

const server = http2.createSecureServer({ key: readFileSync(key), cert: readFileSync(cert), allowHTTP1: true });
server.on('secureConnection', () => conns++);
server.on('request', async (req, res) => {
  const url = new URL(req.url, 'https://x');
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const body = Buffer.concat(chunks);
  const json = (o) => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(o)); };
  switch (url.pathname) {
    case '/echo': return json({ method: req.method, headers: req.headers, bytes: body.length, httpVersion: req.httpVersion });
    case '/upload': return json({ bytes: body.length, sha256: createHash('sha256').update(body).digest('hex') });
    case '/conns': return json({ conns });
    case '/headers': {
      const h = { 'content-type': 'text/plain' };
      for (const n of names) h[n] = n === 'set-cookie' ? ['a=1; Path=/', 'b=2; Secure'] : `value-of-${n}-${'x'.repeat(n.length)}`;
      res.writeHead(200, h);
      return res.end('ok');
    }
    case '/bytes': {
      const n = Number(url.searchParams.get('n') ?? 0);
      res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': n });
      const buf = Buffer.alloc(n);
      for (let i = 0; i < n; i++) buf[i] = i % 251;
      return res.end(buf);
    }
    case '/login': {
      const n = Number(url.searchParams.get('assets') ?? 40);
      let html = '<!doctype html><html><head><meta charset="utf-8"><title>login</title>';
      for (let i = 0; i < n; i++) html += i % 5 === 0 ? `<link rel="stylesheet" href="/asset/${i}.css">` : `<script src="/asset/${i}.js"></script>`;
      html += '</head><body><h1>Sign in</h1></body></html>';
      res.writeHead(200, { 'content-type': 'text/html' });
      return res.end(html);
    }
    default:
      if (url.pathname.startsWith('/asset/')) {
        await wait(Number(delay));
        const css = url.pathname.endsWith('.css');
        res.writeHead(200, { 'content-type': css ? 'text/css' : 'text/javascript' });
        return res.end((css ? '.a{color:red}\n' : 'var x=1;\n').repeat(200));
      }
      res.writeHead(404); res.end('no');
  }
});
server.listen(Number(port), '127.0.0.1', () => console.log('listening'));

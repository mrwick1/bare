// A TCP proxy that delays every chunk, in both directions, by the same time: it adds a round trip of 2*ONE_WAY_MS to a loopback server.
// Order is kept (one fixed delay). It does not read the bytes. For the page-load test of the HTTP/2 client: a local server has no round trip,
// and a handshake costs round trips.
// usage: node delay_proxy.mjs LISTEN_PORT TARGET_PORT ONE_WAY_MS
import net from 'node:net';

const [listen, target, ms] = process.argv.slice(2).map(Number);
const pipe = (from, to) => {
  from.on('data', d => setTimeout(() => to.writable && to.write(d), ms));
  from.on('end', () => setTimeout(() => to.end(), ms));
  from.on('error', () => to.destroy());
};
net.createServer(c => {
  const s = net.connect(target, '127.0.0.1');
  c.setNoDelay(true); s.setNoDelay(true);
  pipe(c, s); pipe(s, c);
  c.on('close', () => s.destroy()); s.on('close', () => c.destroy());
}).listen(listen, '127.0.0.1', () => console.log('listening'));

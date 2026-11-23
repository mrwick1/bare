// The chat server of the fixture. One HTTP server gives:
// - `public/`: the Socket.IO app (`index.html`) and the plain WebSocket app (`plain.html`).
// - Socket.IO on `/socket.io/`: rooms, names, history, typing, presence, acks and images (binary).
// - Plain WebSocket on `/ws` (`ws`, permessage-deflate on): JSON messages, binary images, a ping every PING_MS.
// - A bot in each room: it answers `echo <text>` with the text, `image` with 4 bytes, and `big <n>` with n characters.
// - `GET /log`: what the server saw, as JSON, for a step that checks the server side.
// - `GET /login`: sets an HttpOnly session cookie and goes to `plain.html?auth=1`, whose socket (`/ws?auth=1`) needs the cookie.
//   node server.mjs [PORT]
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { Server } from 'socket.io';
import { WebSocketServer } from 'ws';

const port = Number(process.argv[2] || 8790);
const PING_MS = Number(process.env.PING_MS || 5000);
const dir = path.join(path.dirname(new URL(import.meta.url).pathname), 'public');
const log = [];
const note = (e) => { log.push({ t: Date.now(), ...e }); if (log.length > 500) log.shift(); };
const types = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css' };

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/login') {
    // A login sets an HttpOnly session cookie, as a real app does; `/ws?auth=1` needs it.
    res.writeHead(302, { 'set-cookie': 'sid=s3cret; HttpOnly; SameSite=Lax; Path=/', location: '/plain.html?auth=1' });
    return res.end();
  }
  if (url.pathname === '/log') {
    res.writeHead(200, { 'content-type': 'application/json' });
    return res.end(JSON.stringify(log));
  }
  const file = path.join(dir, url.pathname === '/' ? 'index.html' : path.normalize(url.pathname));
  if (!file.startsWith(dir) || !fs.existsSync(file)) { res.writeHead(404); return res.end('not found'); }
  res.writeHead(200, { 'content-type': types[path.extname(file)] || 'application/octet-stream' });
  fs.createReadStream(file).pipe(res);
});

// Socket.IO. `destroyUpgrade: false` leaves the upgrades of `/ws` to the plain server.
const io = new Server(server, { destroyUpgrade: false, pingInterval: PING_MS, pingTimeout: 4000, perMessageDeflate: { threshold: 256 } });
const history = new Map();
const users = (room) => [...(io.sockets.adapter.rooms.get(room) || [])].map((id) => io.sockets.sockets.get(id)?.data.name).filter(Boolean).concat('bot');
function say(room, from, text) {
  const m = { id: Math.random().toString(36).slice(2, 10), from, text, at: Date.now() };
  const h = history.get(room) || [];
  h.push(m); if (h.length > 50) h.shift();
  history.set(room, h);
  io.to(room).emit('msg', m);
  return m;
}
function bot(room, text) {
  if (text.startsWith('echo ')) setTimeout(() => say(room, 'bot', text.slice(5)), 50);
  else if (text === 'image') setTimeout(() => io.to(room).emit('image', { from: 'bot', bytes: Buffer.from([137, 80, 78, 71]) }), 50);
  else if (text.startsWith('big ')) setTimeout(() => say(room, 'bot', 'b'.repeat(Number(text.slice(4)) || 0)), 50);
}
io.on('connection', (socket) => {
  note({ io: 'connect', transport: socket.conn.transport.name });
  socket.conn.on('upgrade', (t) => note({ io: 'upgrade', transport: t.name }));
  socket.on('join', ({ name, room }, ack) => {
    socket.data = { name: String(name).slice(0, 40), room: String(room).slice(0, 40) };
    socket.join(socket.data.room);
    note({ io: 'join', name: socket.data.name, room: socket.data.room });
    ack({ history: history.get(socket.data.room) || [], users: users(socket.data.room) });
    socket.to(socket.data.room).emit('presence', { users: users(socket.data.room), joined: socket.data.name });
  });
  socket.on('msg', (text, ack) => {
    if (!socket.data.room) return ack?.({ error: 'join first' });
    const m = say(socket.data.room, socket.data.name, String(text).slice(0, 100000));
    note({ io: 'msg', from: m.from, len: m.text.length });
    ack?.({ id: m.id });
    bot(socket.data.room, m.text);
  });
  socket.on('typing', (on) => socket.to(socket.data.room).emit('typing', { name: socket.data.name, on: !!on }));
  socket.on('image', (bytes, ack) => {
    note({ io: 'image', from: socket.data.name, bytes: bytes?.length ?? -1, binary: Buffer.isBuffer(bytes) });
    socket.to(socket.data.room).emit('image', { from: socket.data.name, bytes });
    ack?.({ bytes: bytes?.length ?? -1 });
  });
  socket.on('disconnect', (why) => {
    note({ io: 'disconnect', why });
    if (socket.data.room) io.to(socket.data.room).emit('presence', { users: users(socket.data.room), left: socket.data.name });
  });
});

// Plain WebSocket on `/ws`.
const wss = new WebSocketServer({ noServer: true, perMessageDeflate: { threshold: 256 } });
server.on('upgrade', (req, sock, head) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname !== '/ws') return;
  if (url.searchParams.has('auth') && !/(^|; )sid=s3cret(;|$)/.test(req.headers.cookie || '')) {
    note({ ws: 'refused', cookie: req.headers.cookie || '' });
    sock.end('HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n');
    return;
  }
  wss.handleUpgrade(req, sock, head, (ws) => wss.emit('connection', ws, req));
});
wss.on('connection', (ws, req) => {
  note({ ws: 'open', protocol: ws.protocol, extensions: ws.extensions || req.headers['sec-websocket-extensions'] || '', origin: req.headers.origin || '', cookie: req.headers.cookie || '' });
  let alive = true;
  ws.on('pong', () => { alive = true; });
  const timer = setInterval(() => {
    if (!alive) { note({ ws: 'dead' }); return ws.terminate(); }
    alive = false;
    ws.ping();
  }, PING_MS);
  ws.send(JSON.stringify({ type: 'hello', protocol: ws.protocol }));
  ws.on('message', (data, binary) => {
    if (binary) {
      note({ ws: 'binary', bytes: data.length });
      return ws.send(data, { binary: true });
    }
    const m = JSON.parse(String(data));
    note({ ws: 'msg', type: m.type, len: String(m.text || '').length });
    if (m.type === 'echo') ws.send(JSON.stringify({ type: 'echo', text: m.text }));
    else if (m.type === 'big') ws.send(JSON.stringify({ type: 'echo', text: 'b'.repeat(m.n) }));
    else if (m.type === 'kick') ws.close(4000, 'kicked');
    else if (m.type === 'drop') ws.terminate();
  });
  ws.on('close', (code, reason) => { clearInterval(timer); note({ ws: 'close', code, reason: String(reason) }); });
});

server.listen(port, '127.0.0.1', () => console.log(`chat on http://127.0.0.1:${port}/`));

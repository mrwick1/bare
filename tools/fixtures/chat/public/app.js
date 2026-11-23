// The Socket.IO chat client. It starts with HTTP long-polling and upgrades to WebSocket, as Socket.IO does by default.
(function () {
  var $ = function (id) { return document.getElementById(id); };
  var me = '', reconnects = 0, typingOff = null, sent = 0;
  var socket = io({ autoConnect: false, reconnectionDelay: 200, reconnectionDelayMax: 1000 });
  window.chat = { socket: socket, acks: [], errors: [] };

  function add(m) {
    var li = document.createElement('li');
    if (m.from === me) li.className = 'mine';
    var b = document.createElement('b');
    b.textContent = m.from;
    li.appendChild(b);
    li.appendChild(document.createTextNode(m.text.length > 80 ? 'text of ' + m.text.length + ' chars' : m.text));
    li.dataset.id = m.id;
    $('list').appendChild(li);
    $('list').scrollTop = $('list').scrollHeight;
  }
  function users(list) {
    $('users').textContent = '';
    list.forEach(function (u) { var li = document.createElement('li'); li.textContent = u; $('users').appendChild(li); });
  }
  function status() {
    var on = socket.connected;
    $('status').textContent = on ? 'online' : 'offline';
    $('status').className = 'status ' + (on ? 'on' : 'off');
    $('transport').textContent = on ? socket.io.engine.transport.name : '';
  }
  function join() {
    me = $('name').value;
    socket.emit('join', { name: me, room: $('room').value }, function (r) {
      $('list').textContent = '';
      r.history.forEach(add);
      users(r.users);
      document.title = 'chat ' + me;
    });
  }

  socket.on('connect', function () { status(); if (me) join(); });
  socket.on('connect_error', function (e) { window.chat.errors.push(String(e && e.message)); status(); });
  socket.on('disconnect', function () { status(); });
  socket.io.on('reconnect', function () { reconnects++; $('reconnects').textContent = 'reconnects ' + reconnects; });
  socket.io.on('open', function () {
    socket.io.engine.on('upgrade', status);
  });
  socket.on('msg', add);
  socket.on('presence', function (p) { users(p.users); });
  socket.on('typing', function (t) { $('typing').textContent = t.on ? t.name + ' is typing' : ''; });
  socket.on('image', function (m) {
    var bytes = new Uint8Array(m.bytes);
    var li = document.createElement('li');
    li.textContent = m.from + ': ' + bytes.length + ' bytes [' + Array.prototype.join.call(bytes, '.') + ']';
    $('images').appendChild(li);
  });

  $('join').onclick = function () { if (socket.connected) join(); else { me = $('name').value; socket.connect(); } };
  $('drop').onclick = function () { socket.io.engine.close(); };
  $('text').addEventListener('input', function () {
    socket.emit('typing', true);
    clearTimeout(typingOff);
    typingOff = setTimeout(function () { socket.emit('typing', false); }, 500);
  });
  $('form').addEventListener('submit', function (e) {
    e.preventDefault();
    var text = $('text').value;
    if (!text) return;
    var n = ++sent;
    socket.emit('msg', text, function (r) { window.chat.acks.push(n + ':' + (r.id ? 'ok' : r.error)); });
    $('text').value = '';
    socket.emit('typing', false);
  });
  $('image').onclick = function () {
    var bytes = new Uint8Array([71, 73, 70, 56, 57, 97]);
    socket.emit('image', bytes.buffer, function (r) { window.chat.acks.push('image:' + r.bytes); });
  };
})();

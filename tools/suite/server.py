#!/usr/bin/env python3
"""The fixture app of the suite benchmark (docs/design-suite.md, section 5.1). Python standard library, loopback only.

usage: server.py [--port N] [--login-ms N] [--js-kb N] [--css-kb N]
  --port 0 (the default) takes a free port. The first line on stdout is `listening PORT`.
  --login-ms N   the server waits N ms in `POST /login` (the cost of a slow login, with no CPU on the client)
  --js-kb, --css-kb   the size of the two shared assets (defaults 300 and 60)

Routes: GET /login (form), POST /login (cookie `sid`, redirect to /home; a wrong login is a 401 page), POST /logout (ends the server
session of that cookie), GET /home, /p1 to /p8 and /notes (redirect to /login with no valid session), GET and POST /api/notes, GET
/api/items, GET /static/app.css and /static/app.js (`public, immutable`), GET /__stats (counters as JSON).
The head and the body go out in one send: two sends meet Nagle and the delayed ACK, and each response waits 41 ms.
"""
import hashlib
import json
import secrets
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

args = sys.argv[1:]


def opt(name, default):
    return int(args[args.index(name) + 1]) if name in args else default


PORT, LOGIN_MS, JS_KB, CSS_KB = opt("--port", 0), opt("--login-ms", 0), opt("--js-kb", 300), opt("--css-kb", 60)
EMAIL, PASSWORD = "suite.user@example.test", "suite pass 1!"
sessions, notes, lock = {}, [], threading.Lock()
stats = {"login_posts": 0, "logins_ok": 0, "logouts": 0, "requests": 0, "static": 0, "api": 0}


def make_css(kb):
    out, size, i = [], 0, 0
    while size < kb * 1024:
        rule = f".c{i}{{color:#{i * 7 % 4096:03x};margin:{i % 9}px}} .c{i} .x{i % 13}:hover{{padding:{i % 5}px}}\n"
        out.append(rule)
        size += len(rule)
        i += 1
    return "body{font:16px sans-serif} nav a{margin-right:8px} table{border-collapse:collapse} td{padding:2px 6px}\n" + "".join(out)


def make_js(kb):
    # Many functions that never run, as in a real bundle, and a small `boot` that runs on each page.
    out, size, i = [], 0, 0
    while size < kb * 1024:
        fn = f"function f{i}(a,b){{var r=[];for(var k=0;k<a;k++){{if(k%{i % 7 + 2}==0){{r.push(b+k*{i})}}}}return r.map(function(x){{return x*{i % 5 + 1}}})}}\n"
        out.append(fn)
        size += len(fn)
        i += 1
    boot = """
function fill(id,items){var el=document.getElementById(id);items.forEach(function(t){var li=document.createElement('li');li.textContent=t;el.appendChild(li)})}
function boot(){
 var items=document.getElementById('items');
 if(items)fetch('/api/items?page='+encodeURIComponent(location.pathname)).then(function(r){return r.json()}).then(function(d){
  fill('items',d.items);document.getElementById('count').textContent=String(d.items.length)});
 var list=document.getElementById('notes');
 if(list){
  fetch('/api/notes').then(function(r){return r.json()}).then(function(d){fill('notes',d.notes)});
  document.getElementById('add').addEventListener('click',function(){
   var f=document.getElementById('note'),t=f.value;
   fetch('/api/notes',{method:'POST',body:t}).then(function(r){return r.json()}).then(function(d){fill('notes',[t]);f.value=''})})}}
if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',boot);else boot();
"""
    return "".join(out) + boot


CSS, JS = make_css(CSS_KB).encode(), make_js(JS_KB).encode()
ETAG = '"' + hashlib.sha1(CSS + JS).hexdigest()[:12] + '"'
PAGES = [f"p{i}" for i in range(1, 9)]
NAV = (
    "<nav><a href='/home'>Home</a>"
    + "".join(f"<a href='/{p}'>Page {p[1:]}</a>" for p in PAGES)
    + "<a href='/notes'>Notes</a><form method=post action=/logout style='display:inline'><button>Sign out</button></form></nav>"
)
HEAD = "<!doctype html><meta charset=utf-8><link rel=stylesheet href='/static/app.css?v=1'><script src='/static/app.js?v=1' defer></script>"


def page(name):
    rows = "".join(f"<tr><td>{name}-{r}</td><td class='c{r}'>value {r}</td></tr>" for r in range(60))
    return (HEAD + f"<title>{name}</title>" + NAV + f"<h1>Heading {name}</h1><p>Items: <span id=count>0</span></p><ul id=items></ul><table>{rows}</table>").encode()


def notes_page():
    return (HEAD + "<title>notes</title>" + NAV + "<h1>Heading notes</h1><ul id=notes></ul><label>Note <input id=note></label><button id=add type=button>Add note</button>").encode()


def login_page(note=""):
    return (
        HEAD
        + "<title>Sign in</title><h1>Sign in</h1>"
        + note
        + "<form method=post action=/login><label>Email <input name=email type=email></label>"
        + "<label>Password <input name=password type=password></label><button type=submit>Sign in</button></form>"
    ).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    wbufsize = 1 << 20  # the head and the body in one send

    def log_message(self, *a):
        pass

    def send(self, code, body, ctype="text/html; charset=utf-8", extra=()):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()

    def user(self):
        cookie = self.headers.get("Cookie", "")
        sid = next((p.split("=", 1)[1] for p in (x.strip() for x in cookie.split(";")) if p.startswith("sid=")), None)
        with lock:
            return sessions.get(sid)

    def do_GET(self):
        u = urlparse(self.path)
        with lock:
            stats["requests"] += 1
        if u.path == "/__stats":
            with lock:
                return self.send(200, json.dumps(stats).encode(), "application/json")
        if u.path.startswith("/static/"):
            with lock:
                stats["static"] += 1
            body, ctype = (CSS, "text/css") if u.path.endswith(".css") else (JS, "text/javascript")
            if self.headers.get("If-None-Match") == ETAG:
                return self.send(304, b"", ctype, [("ETag", ETAG)])
            return self.send(200, body, ctype, [("Cache-Control", "public, max-age=31536000, immutable"), ("ETag", ETAG)])
        if u.path in ("/", "/login"):
            return self.send(200, login_page())
        if self.user() is None:
            return self.send(303, b"", extra=[("Location", "/login")])
        if u.path == "/api/items":
            with lock:
                stats["api"] += 1
            return self.send(200, json.dumps({"items": [f"item {i}" for i in range(20)]}).encode(), "application/json", [("Cache-Control", "no-store")])
        if u.path == "/api/notes":
            with lock:
                stats["api"] += 1
                body = json.dumps({"notes": list(notes)}).encode()
            return self.send(200, body, "application/json", [("Cache-Control", "no-store")])
        if u.path == "/home":
            return self.send(200, page("home"))
        if u.path == "/notes":
            return self.send(200, notes_page())
        if u.path[1:] in PAGES:
            return self.send(200, page(u.path[1:]))
        self.send(404, b"not found")

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0"))).decode()
        if self.path == "/login":
            with lock:
                stats["login_posts"] += 1
            form = {k: v[0] for k, v in parse_qs(body).items()}
            time.sleep(LOGIN_MS / 1000)
            if form.get("email") != EMAIL or form.get("password") != PASSWORD:
                return self.send(401, login_page("<p>Sign-in failed</p>"))
            sid = secrets.token_hex(16)
            with lock:
                sessions[sid] = EMAIL
                stats["logins_ok"] += 1
            return self.send(303, b"", extra=[("Set-Cookie", f"sid={sid}; Path=/; HttpOnly; SameSite=Lax"), ("Location", "/home")])
        if self.path == "/logout":
            cookie = self.headers.get("Cookie", "")
            sid = next((p.split("=", 1)[1] for p in (x.strip() for x in cookie.split(";")) if p.startswith("sid=")), None)
            with lock:
                sessions.pop(sid, None)
                stats["logouts"] += 1
            return self.send(303, b"", extra=[("Set-Cookie", "sid=; Path=/; Max-Age=0"), ("Location", "/login")])
        if self.path == "/api/notes":
            if self.user() is None:
                return self.send(401, b"{}", "application/json")
            with lock:
                stats["api"] += 1
                notes.append(body)
            return self.send(200, b"{}", "application/json", [("Cache-Control", "no-store")])
        self.send(404, b"not found")


class Server(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 128


server = Server(("127.0.0.1", PORT), Handler)
print(f"listening {server.server_address[1]}", flush=True)
server.serve_forever()
